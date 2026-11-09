//! Native EdgeClaw provider composition.
//!
//! This is the Rust owner for the service/provider pair constructed by
//! `createEdgeClawMemoryProviderFromConfig.ts` and `EdgeClawMemoryProvider.ts`.
//! One shared runtime owns retrieval, turn capture, maintenance, management,
//! case-trace correlation and the repository lifetime.

use super::{EdgeClawFileStores, FileMemoryError, file_memory::helpers::hash};
use crate::{
    metadata::js_trim,
    runtime::{
        context::memory::{
            MemoryCaptureFuture, MemoryCaptureTurnInput, MemoryDiagnostic, MemoryFuture,
            MemoryResolver, MemoryRetrieveInput, MemoryRetrieveResult,
        },
        memory::{
            MemoryBundleError, MemoryDisposeFn, MemoryFuture as MaintenanceFuture,
            MemoryMaintenanceService, MemoryManagementService, MemoryProviderError,
            MemoryProviderFactoryFn, ProjectMemoryBundleOptions, ProjectMemoryProvider,
        },
        project::RuntimeError,
    },
};
use pilotdeck_protocol::{JsObject, JsString, JsValue, stringify_json};
use regex::Regex;
use std::{
    collections::{HashMap, HashSet},
    future::Future,
    path::{Path, PathBuf},
    pin::Pin,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use tokio::sync::Mutex as AsyncMutex;

pub type NativeEdgeClawLlmFuture =
    Pin<Box<dyn Future<Output = Result<JsValue, String>> + Send + 'static>>;

/// Structured LLM port used by retrieval, indexing and Dream. The production
/// host adapts its configured model once; this module owns the prompts' data
/// contract, validation, storage transactions and failure recovery.
pub trait NativeEdgeClawLlm: Send + Sync {
    fn execute(&self, operation: &'static str, input: JsValue) -> NativeEdgeClawLlmFuture;
}

pub type NativeEdgeClawClock = Arc<dyn Fn() -> String + Send + Sync>;

#[derive(Clone)]
pub struct NativeProjectMemoryOwner {
    pub resolver: Arc<dyn MemoryResolver>,
    pub maintenance: Arc<dyn MemoryMaintenanceService>,
    pub management: Arc<dyn MemoryManagementService>,
    pub disposer: MemoryDisposeFn,
    pub retrieval_timeout_ms: f64,
    pub has_llm: bool,
    pub enabled: bool,
}

#[derive(Clone)]
pub struct NativeEdgeClawOptions {
    pub root_dir: PathBuf,
    pub general_workspace: PathBuf,
    pub source: String,
    pub llm: Option<Arc<dyn NativeEdgeClawLlm>>,
    pub default_indexing_settings: super::IndexingSettings,
    pub capture_strategy: CaptureStrategy,
    pub include_assistant: bool,
    pub max_message_chars: usize,
    pub clock: NativeEdgeClawClock,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CaptureStrategy {
    LastTurn,
    FullSession,
}

impl NativeEdgeClawOptions {
    pub fn new(root_dir: impl Into<PathBuf>, general_workspace: impl Into<PathBuf>) -> Self {
        Self {
            root_dir: root_dir.into(),
            general_workspace: general_workspace.into(),
            source: "pilotdeck".into(),
            llm: None,
            default_indexing_settings: super::IndexingSettings {
                reasoning_mode: "answer_first".into(),
                auto_index_interval_minutes: 30,
                auto_dream_interval_minutes: 60,
            },
            capture_strategy: CaptureStrategy::LastTurn,
            include_assistant: true,
            max_message_chars: 6_000,
            clock: Arc::new(now_iso),
        }
    }

    pub fn with_llm(mut self, llm: Arc<dyn NativeEdgeClawLlm>) -> Self {
        self.llm = Some(llm);
        self
    }

    pub fn with_indexing_settings(mut self, settings: super::IndexingSettings) -> Self {
        self.default_indexing_settings = settings;
        self
    }

    pub fn with_capture_policy(
        mut self,
        capture_strategy: CaptureStrategy,
        include_assistant: bool,
        max_message_chars: usize,
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

/// Ready-to-install `ProjectMemoryBundleOptions.provider_factory`. Keeping the
/// root selection outside this module prevents a library constructor from
/// reading process environment or touching a user's live memory directory.
pub fn native_provider_factory(options: NativeEdgeClawOptions) -> MemoryProviderFactoryFn {
    Arc::new(move |bundle| create_project_provider(&bundle, options.clone()))
}

/// Build one production project owner from the already validated config
/// snapshot. Model execution remains an injected port: this function neither
/// reads process credentials nor treats local keyword matching as an LLM.
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
    let provider = value_text(memory.get("provider")).unwrap_or_else(|| "edgeclaw".into());
    if provider != "edgeclaw" {
        return Ok(None);
    }

    let root_dir = value_text(memory.get("rootDir"))
        .filter(|value| !js_trim(value).is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| pilot_home.as_ref().join("memory"));
    let mut settings = super::IndexingSettings {
        reasoning_mode: "answer_first".into(),
        auto_index_interval_minutes: 30,
        auto_dream_interval_minutes: 60,
    };
    if let Some(schedule) = memory.get("schedule").and_then(JsValue::as_object) {
        if let Some(mode) = value_text(schedule.get("reasoningMode"))
            && matches!(mode.as_str(), "answer_first" | "accuracy_first")
        {
            settings.reasoning_mode = mode;
        }
        if let Some(value) = non_negative_integer(schedule.get("autoIndexIntervalMinutes")) {
            settings.auto_index_interval_minutes = value;
        }
        if let Some(value) = non_negative_integer(schedule.get("autoDreamIntervalMinutes")) {
            settings.auto_dream_interval_minutes = value;
        }
    }
    let capture_strategy = match value_text(memory.get("captureStrategy")).as_deref() {
        Some("full_session") => CaptureStrategy::FullSession,
        _ => CaptureStrategy::LastTurn,
    };
    let include_assistant = memory
        .get("includeAssistant")
        .and_then(JsValue::as_bool)
        .unwrap_or(true);
    let max_message_chars = non_negative_integer(memory.get("maxMessageChars"))
        .map(|value| value as usize)
        .unwrap_or(6_000);
    let retrieval_timeout_ms = memory
        .get("retrievalTimeoutMs")
        .and_then(JsValue::as_number)
        .filter(|value| value.is_finite() && *value > 0.0)
        .unwrap_or(30_000.0);
    let has_llm = llm.is_some();
    let mut options = NativeEdgeClawOptions::new(root_dir, pilot_home.as_ref())
        .with_indexing_settings(settings)
        .with_capture_policy(capture_strategy, include_assistant, max_message_chars);
    if let Some(llm) = llm {
        options = options.with_llm(llm);
    }
    let runtime = NativeEdgeClawRuntime::open(project_root, options)?;
    let resolver: Arc<dyn MemoryResolver> = Arc::new(runtime.clone());
    let maintenance: Arc<dyn MemoryMaintenanceService> = Arc::new(runtime.clone());
    let management: Arc<dyn MemoryManagementService> = Arc::new(runtime.clone());
    let dispose_runtime = runtime;
    let disposer: MemoryDisposeFn = Arc::new(move || {
        let runtime = dispose_runtime.clone();
        Box::pin(async move {
            runtime
                .close()
                .map_err(|error| RuntimeError::new(String::from_utf16_lossy(error.message.units())))
        })
    });
    Ok(Some(NativeProjectMemoryOwner {
        resolver,
        maintenance,
        management,
        disposer,
        retrieval_timeout_ms,
        has_llm,
        enabled: true,
    }))
}

#[derive(Clone)]
pub struct NativeEdgeClawRuntime {
    inner: Arc<RuntimeInner>,
}

struct RuntimeInner {
    stores: Arc<EdgeClawFileStores>,
    source: String,
    llm: Option<Arc<dyn NativeEdgeClawLlm>>,
    default_indexing_settings: super::IndexingSettings,
    capture_strategy: CaptureStrategy,
    include_assistant: bool,
    max_message_chars: usize,
    clock: NativeEdgeClawClock,
    pending_retrievals: Mutex<HashMap<String, PendingRetrieval>>,
    recall_cache: Mutex<HashMap<String, RecallCacheEntry>>,
    maintenance_gate: AsyncMutex<()>,
    maintenance_requested: AtomicBool,
    closed: AtomicBool,
}

#[derive(Clone)]
struct PendingRetrieval {
    query: String,
    started_at: String,
    result: MemoryRetrieveResult,
}

#[derive(Clone)]
struct RecallCacheEntry {
    expires_at: Instant,
    result: MemoryRetrieveResult,
}

#[derive(Clone)]
struct DreamCluster {
    member_ids: Vec<JsString>,
    reason: String,
}

struct GeneralMergeGroup {
    keeper_project_id: String,
    duplicate_project_ids: Vec<String>,
}

struct DreamStageGuard(Option<super::storage::snapshots::MemoryRepositoryStage>);

impl DreamStageGuard {
    fn get(&self) -> &super::storage::snapshots::MemoryRepositoryStage {
        self.0.as_ref().expect("dream stage is present")
    }

    fn dispose(mut self) -> Result<(), FileMemoryError> {
        self.0.take().expect("dream stage is present").dispose()
    }
}

impl Drop for DreamStageGuard {
    fn drop(&mut self) {
        if let Some(stage) = self.0.take() {
            let _ = stage.dispose();
        }
    }
}

/// Construct the native service/provider pair only for enabled EdgeClaw.
/// The caller owns the root selection because the trusted config layer owns
/// `memory.rootDir`; no environment or credential is read here.
pub fn create_project_provider(
    bundle: &ProjectMemoryBundleOptions,
    options: NativeEdgeClawOptions,
) -> Result<Option<ProjectMemoryProvider>, MemoryBundleError> {
    if !bundle
        .config
        .as_ref()
        .is_some_and(|config| config.enabled && config.provider == "edgeclaw")
    {
        return Ok(None);
    }
    let runtime = NativeEdgeClawRuntime::open(&bundle.project_root, options)
        .map_err(MemoryBundleError::Factory)?;
    let memory: Arc<dyn MemoryResolver> = Arc::new(runtime.clone());
    let maintenance: Arc<dyn MemoryMaintenanceService> = Arc::new(runtime.clone());
    let management: Arc<dyn MemoryManagementService> = Arc::new(runtime.clone());
    let dispose_runtime = runtime.clone();
    let dispose: MemoryDisposeFn = Arc::new(move || {
        let runtime = dispose_runtime.clone();
        Box::pin(async move {
            runtime
                .close()
                .map_err(|error| RuntimeError::new(String::from_utf16_lossy(error.message.units())))
        })
    });
    Ok(Some(ProjectMemoryProvider {
        memory,
        maintenance: Some(maintenance),
        management: Some(management),
        dispose: Some(dispose),
    }))
}

impl NativeEdgeClawRuntime {
    pub fn open(
        project_root: impl AsRef<Path>,
        options: NativeEdgeClawOptions,
    ) -> Result<Self, MemoryProviderError> {
        let project_root = project_root.as_ref().to_path_buf();
        let data = options
            .root_dir
            .join("workspaces")
            .join(hash(&project_root.to_string_lossy()));
        let clock = Arc::clone(&options.clock);
        let stores = EdgeClawFileStores::with_workspace_clock(
            data.join("control.sqlite"),
            data.join("memory"),
            options.root_dir.join("global"),
            &project_root,
            options.general_workspace,
            Arc::clone(&clock),
        )
        .map_err(provider_error)?;
        stores
            .storage
            .set_pipeline_state(
                "workspaceDir",
                &JsValue::from(project_root.to_string_lossy().as_ref()),
            )
            .map_err(|error| MemoryProviderError::new(error.to_string()))?;
        Ok(Self {
            inner: Arc::new(RuntimeInner {
                stores: Arc::new(stores),
                source: options.source,
                llm: options.llm,
                default_indexing_settings: options.default_indexing_settings,
                capture_strategy: options.capture_strategy,
                include_assistant: options.include_assistant,
                max_message_chars: options.max_message_chars,
                clock,
                pending_retrievals: Mutex::new(HashMap::new()),
                recall_cache: Mutex::new(HashMap::new()),
                maintenance_gate: AsyncMutex::new(()),
                maintenance_requested: AtomicBool::new(false),
                closed: AtomicBool::new(false),
            }),
        })
    }

    pub fn close(&self) -> Result<(), MemoryProviderError> {
        if self.inner.closed.swap(true, Ordering::AcqRel) {
            return Ok(());
        }
        self.inner.stores.close().map_err(provider_error)
    }

    fn ensure_open(&self) -> Result<(), String> {
        if self.inner.closed.load(Ordering::Acquire) {
            Err("EdgeClaw memory provider is closed.".into())
        } else {
            Ok(())
        }
    }

    fn manifest_for_route(&self, route: &str) -> Result<Vec<JsValue>, String> {
        let kinds = match route {
            "user" => vec![JsValue::from("user")],
            "project" | "mix" => {
                vec![JsValue::from("project"), JsValue::from("feedback")]
            }
            _ => return Ok(Vec::new()),
        };
        let mut options = JsObject::new();
        options.insert("kinds".into(), JsValue::Array(kinds));
        options.insert(
            "scope".into(),
            JsValue::from(if route == "user" { "global" } else { "project" }),
        );
        options.insert("limit".into(), JsValue::Number(200.0));
        options.insert("offset".into(), JsValue::Number(0.0));
        options.insert("includeDeprecated".into(), JsValue::Bool(false));
        let value = if route == "user" {
            self.inner.stores.list_global_memory_entries(&options)
        } else {
            self.inner
                .stores
                .workspace_memory
                .list_memory_entries(&options)
        }
        .map_err(|error| error.to_string())?;
        let mut entries = match &value {
            JsValue::Array(entries) => entries.clone(),
            _ => Vec::new(),
        };
        entries.truncate(200);
        Ok(entries)
    }

    async fn retrieve_now(
        &self,
        input: &MemoryRetrieveInput,
    ) -> Result<MemoryRetrieveResult, String> {
        self.ensure_open()?;
        if input.signal.is_cancelled() {
            return Err("Memory retrieval aborted.".into());
        }
        // Reading the manifest also completes the file store's lazy layout
        // repair. Compute the cache snapshot afterwards so the first recall
        // and its immediate repeat share the same repository version.
        let planning_manifest = self
            .inner
            .stores
            .list_memory_entries(200)
            .map_err(|error| error.to_string())?;
        let cache_key = recall_cache_key(&self.inner.stores, input, &planning_manifest)?;
        if let Some(cached) = self
            .inner
            .recall_cache
            .lock()
            .expect("recall cache mutex poisoned")
            .get(&cache_key)
            .filter(|entry| entry.expires_at > Instant::now())
            .cloned()
        {
            return Ok(mark_recall_cache_hit(cached.result));
        }
        let started_at = (self.inner.clock)();
        let start_instant = Instant::now();
        let retrieval_mode = "auto".to_string();

        let canonical = canonical_messages(&input.recent_messages, true);
        let recent_user = recent_user_messages(&canonical);
        let recent_user_count = recent_user.len();

        // The TS route gate fails closed when its model call is unavailable.
        // LLM failures fall back to route="none" (empty recall), without returning Err.
        let mut route = "none".to_owned();
        let mut gate_debug = JsValue::Null;
        let mut legacy_plan_ids = Vec::<JsString>::new();
        if let Some(llm) = &self.inner.llm {
            let recent_messages_json = canonical
                .iter()
                .rev()
                .take(4)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .map(|msg| {
                    let role = msg
                        .get("role")
                        .and_then(JsValue::as_string)
                        .and_then(|s| s.to_utf8().ok())
                        .unwrap_or_else(|| "user".into());
                    let content = msg
                        .get("content")
                        .and_then(JsValue::as_string)
                        .and_then(|s| s.to_utf8().ok())
                        .unwrap_or_default();
                    object(&[
                        ("role", JsValue::from(role.as_str())),
                        (
                            "content",
                            JsValue::from(truncate_for_prompt(&content, 220).as_str()),
                        ),
                    ])
                })
                .collect::<Vec<_>>();

            let route_response = llm
                .execute(
                    "decide_file_memory_route",
                    object(&[
                        ("query", JsValue::from(input.query.as_str())),
                        ("recent_messages", JsValue::Array(recent_messages_json)),
                    ]),
                )
                .await;
            match route_response {
                Ok(response) => {
                    if let Some(next) = value_text(response.get("route"))
                        && matches!(next.as_str(), "none" | "user" | "project" | "mix")
                    {
                        route = next;
                    }
                    gate_debug = response;
                }
                Err(error) => {
                    if error.contains("Unsupported native memory model operation") {
                        // Fallback to legacy single-plan operation for scripted/mocked tests
                        let plan_response = llm
                            .execute(
                                "memory_retrieval_plan",
                                object(&[
                                    ("query", JsValue::from(input.query.as_str())),
                                    (
                                        "recentMessages",
                                        JsValue::Array(canonical_messages(
                                            &input.recent_messages,
                                            true,
                                        )),
                                    ),
                                    ("workspaceHint", JsValue::from(input.project_root.as_str())),
                                    ("manifest", JsValue::Array(planning_manifest.clone())),
                                ]),
                            )
                            .await;
                        match plan_response {
                            Ok(response) => {
                                if let Some(next) = value_text(response.get("route"))
                                    && matches!(next.as_str(), "none" | "user" | "project" | "mix")
                                {
                                    route = next;
                                }
                                if let Some(JsValue::Array(ids)) = response.get("selectedIds") {
                                    for id in ids.iter().filter_map(JsValue::as_string) {
                                        legacy_plan_ids.push(id.clone());
                                    }
                                }
                                gate_debug = response;
                            }
                            Err(plan_err) => {
                                eprintln!("[clawxmemory] file memory gate fallback: {plan_err}");
                                route = "none".to_owned();
                                gate_debug = object(&[("error", string(plan_err))]);
                            }
                        }
                    } else {
                        // Aligns with TS llm-extraction.ts:2911-2913: catch error and fallback to "none"
                        eprintln!("[clawxmemory] file memory gate fallback: {error}");
                        route = "none".to_owned();
                        gate_debug = object(&[("error", string(error))]);
                    }
                }
            }
        }

        let needs_user = matches!(route.as_str(), "user" | "mix");
        let needs_project = matches!(route.as_str(), "project" | "mix");
        let user_summary = if needs_user {
            Some(
                self.inner
                    .stores
                    .get_user_summary()
                    .map_err(|error| error.to_string())?,
            )
        } else {
            None
        };

        let general = needs_project && self.inner.stores.get_workspace_mode() == "general";
        let mut selected_project = None;
        let mut selected_project_id = None;
        let mut project_selection_debug = JsValue::Null;
        let mut catalog_count = 0;
        let mut shortlist_count = 0;
        if general {
            let catalog = self
                .inner
                .stores
                .list_readable_project_catalog()
                .map_err(|error| error.to_string())?;
            let catalog_values = if let JsValue::Array(values) = &catalog {
                values.as_slice()
            } else {
                &[]
            };
            catalog_count = catalog_values.len();
            let shortlist = build_project_shortlist(
                catalog_values,
                &input.query,
                &canonical_messages(&input.recent_messages, true),
            );
            shortlist_count = shortlist.len();
            if !shortlist.is_empty()
                && let Some(llm) = &self.inner.llm
            {
                let recent_user_msgs: Vec<JsValue> =
                    canonical_messages(&input.recent_messages, true)
                        .into_iter()
                        .filter(|msg| value_text(msg.get("role")).as_deref() == Some("user"))
                        .map(|msg| {
                            let content = value_text(msg.get("content")).unwrap_or_default();
                            JsValue::from(truncate_for_prompt(&content, 220).as_str())
                        })
                        .collect();
                let recent_user_msgs = if recent_user_msgs.len() > 4 {
                    recent_user_msgs[recent_user_msgs.len() - 4..].to_vec()
                } else {
                    recent_user_msgs
                };

                let shortlist_slice: Vec<JsValue> = shortlist
                    .iter()
                    .map(|project| {
                        let project_id = project
                            .get("projectId")
                            .or_else(|| project.get("project_id"))
                            .cloned()
                            .unwrap_or(JsValue::Null);
                        let project_name = project
                            .get("projectName")
                            .or_else(|| project.get("project_name"))
                            .cloned()
                            .unwrap_or(JsValue::Null);
                        let desc = project
                            .get("description")
                            .and_then(JsValue::as_string)
                            .and_then(|s| s.to_utf8().ok())
                            .unwrap_or_default();
                        let status = project.get("status").cloned().unwrap_or(JsValue::Null);
                        let source_type = project
                            .get("sourceType")
                            .or_else(|| project.get("source_type"))
                            .cloned()
                            .unwrap_or_else(|| JsValue::from("unknown"));
                        let updated_at = project
                            .get("updatedAt")
                            .or_else(|| project.get("updated_at"))
                            .cloned()
                            .unwrap_or(JsValue::Null);
                        let score = project
                            .get("score")
                            .or_else(|| project.get("shortlist_score"))
                            .cloned()
                            .unwrap_or(JsValue::Number(0.0));
                        let exact = project
                            .get("exact")
                            .or_else(|| project.get("shortlist_exact"))
                            .cloned()
                            .unwrap_or(JsValue::Number(0.0));
                        let source = project
                            .get("source")
                            .or_else(|| project.get("shortlist_source"))
                            .cloned()
                            .unwrap_or(JsValue::Null);
                        let matched_text = project
                            .get("matchedText")
                            .or_else(|| project.get("matched_text"))
                            .and_then(JsValue::as_string)
                            .and_then(|s| s.to_utf8().ok())
                            .unwrap_or_default();

                        object(&[
                            ("project_id", project_id),
                            ("project_name", project_name),
                            (
                                "description",
                                JsValue::from(truncate_for_prompt(&desc, 180).as_str()),
                            ),
                            ("status", status),
                            ("source_type", source_type),
                            ("updated_at", updated_at),
                            ("shortlist_score", score),
                            ("shortlist_exact", exact),
                            ("shortlist_source", source),
                            (
                                "matched_text",
                                JsValue::from(truncate_for_prompt(&matched_text, 180).as_str()),
                            ),
                        ])
                    })
                    .collect();

                match llm
                    .execute(
                        "memory_recall_project",
                        object(&[
                            ("query", JsValue::from(input.query.as_str())),
                            ("recent_user_messages", JsValue::Array(recent_user_msgs)),
                            ("shortlist", JsValue::Array(shortlist_slice)),
                        ]),
                    )
                    .await
                {
                    Ok(response) => {
                        let candidate = value_text(response.get("selected_project_id"))
                            .or_else(|| value_text(response.get("selectedProjectId")));
                        if let Some(candidate) = candidate
                            && shortlist.iter().any(|entry| {
                                value_text(entry.get("projectId"))
                                    .or_else(|| value_text(entry.get("project_id")))
                                    .as_deref()
                                    == Some(candidate.as_str())
                            })
                        {
                            selected_project_id = Some(candidate.clone());
                            let project = self
                                .inner
                                .stores
                                .get_readable_project(&candidate)
                                .map_err(|error| error.to_string())?;
                            if project.as_object().is_some() {
                                selected_project = Some(project);
                            }
                        }
                        project_selection_debug = response;
                    }
                    Err(error) => {
                        project_selection_debug = object(&[("error", string(error))]);
                    }
                }
            }
        }

        let project_meta = if general {
            selected_project.clone()
        } else if needs_project {
            let value = self
                .inner
                .stores
                .get_project_meta()
                .map_err(|error| error.to_string())?;
            if value.as_object().is_some() {
                Some(value)
            } else {
                None
            }
        } else {
            None
        };

        let manifest = if route == "none" {
            Vec::new()
        } else if route == "user" {
            self.manifest_for_route("user")?
        } else if general {
            if let Some(project_id) = &selected_project_id {
                let mut options = JsObject::new();
                options.insert(
                    "kinds".into(),
                    JsValue::Array(vec![JsValue::from("project"), JsValue::from("feedback")]),
                );
                options.insert("includeDeprecated".into(), JsValue::Bool(false));
                let entries = self
                    .inner
                    .stores
                    .list_readable_project_entries(project_id, &options)
                    .map_err(|error| error.to_string())?;
                match &entries {
                    JsValue::Array(values) => {
                        let mut values = values.clone();
                        values.truncate(200);
                        values
                    }
                    _ => Vec::new(),
                }
            } else {
                Vec::new()
            }
        } else {
            self.manifest_for_route(&route)?
        };

        let available = manifest
            .iter()
            .filter_map(|record| {
                record
                    .get("relativePath")
                    .and_then(JsValue::as_string)
                    .cloned()
            })
            .collect::<HashSet<_>>();

        let mut selected_ids = Vec::<JsString>::new();
        let mut manifest_selection_debug = JsValue::Null;

        // Consume legacy plan selectedIds if provided
        for id in &legacy_plan_ids {
            if available.contains(id) && !selected_ids.contains(id) {
                selected_ids.push(id.clone());
            }
        }

        let selection_limit = if route == "user" { 1 } else { 5 };

        // Call select_file_manifest_entries as independent second LLM call if not already selected
        if selected_ids.is_empty()
            && !manifest.is_empty()
            && route != "none"
            && let Some(llm) = &self.inner.llm
        {
            let recent_user_slice = recent_user
                .iter()
                .rev()
                .take(4)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .map(|msg| {
                    let content = msg
                        .get("content")
                        .and_then(JsValue::as_string)
                        .and_then(|s| s.to_utf8().ok())
                        .unwrap_or_default();
                    JsValue::from(truncate_for_prompt(&content, 220).as_str())
                })
                .collect::<Vec<_>>();

            let project_meta_val = if let Some(meta) = &project_meta {
                let pid = meta.get("projectId").or_else(|| meta.get("project_id"));
                let pname = meta.get("projectName").or_else(|| meta.get("project_name"));
                let desc = meta
                    .get("description")
                    .and_then(JsValue::as_string)
                    .and_then(|s| s.to_utf8().ok())
                    .unwrap_or_default();
                let status = meta.get("status");
                object(&[
                    ("project_id", pid.cloned().unwrap_or(JsValue::Null)),
                    ("project_name", pname.cloned().unwrap_or(JsValue::Null)),
                    (
                        "description",
                        JsValue::from(truncate_for_prompt(&desc, 180).as_str()),
                    ),
                    ("status", status.cloned().unwrap_or(JsValue::Null)),
                ])
            } else {
                JsValue::Null
            };

            let manifest_slice = manifest
                .iter()
                .take(200)
                .map(|entry| {
                    let path = entry
                        .get("relativePath")
                        .or_else(|| entry.get("id"))
                        .cloned()
                        .unwrap_or(JsValue::Null);
                    let ty = entry.get("type").cloned().unwrap_or(JsValue::Null);
                    let scope = entry.get("scope").cloned().unwrap_or(JsValue::Null);
                    let pid = entry
                        .get("projectId")
                        .or_else(|| entry.get("project_id"))
                        .cloned()
                        .unwrap_or(JsValue::Null);
                    let updated = entry
                        .get("updatedAt")
                        .or_else(|| entry.get("updated_at"))
                        .cloned()
                        .unwrap_or(JsValue::Null);
                    let desc = entry
                        .get("description")
                        .and_then(JsValue::as_string)
                        .and_then(|s| s.to_utf8().ok())
                        .unwrap_or_default();
                    object(&[
                        ("id", path),
                        ("type", ty),
                        ("scope", scope),
                        ("project_id", pid),
                        ("updated_at", updated),
                        (
                            "description",
                            JsValue::from(truncate_for_prompt(&desc, 200).as_str()),
                        ),
                    ])
                })
                .collect::<Vec<_>>();

            let selection_res = llm
                .execute(
                    "select_file_manifest_entries",
                    object(&[
                        ("query", JsValue::from(input.query.as_str())),
                        ("route", JsValue::from(route.as_str())),
                        ("recent_user_messages", JsValue::Array(recent_user_slice)),
                        ("project", project_meta_val),
                        ("manifest", JsValue::Array(manifest_slice)),
                        ("limit", JsValue::Number(selection_limit as f64)),
                    ]),
                )
                .await;
            match selection_res {
                Ok(response) => {
                    let candidates = response
                        .get("selected_ids")
                        .or_else(|| response.get("selectedIds"));
                    if let Some(JsValue::String(s)) = candidates {
                        let t = s
                            .to_utf8()
                            .map(|v| v.trim().to_string())
                            .unwrap_or_default();
                        if !t.is_empty() {
                            selected_ids.push(JsString::from(t));
                        }
                    } else if let Some(JsValue::Array(ids)) = candidates {
                        for s in ids.iter().filter_map(JsValue::as_string) {
                            if let Ok(t) = s.to_utf8() {
                                let trimmed = t.trim().to_string();
                                if !trimmed.is_empty() {
                                    selected_ids.push(JsString::from(trimmed));
                                    if selected_ids.len() >= selection_limit {
                                        break;
                                    }
                                }
                            }
                        }
                    }
                    if selected_ids.len() > selection_limit {
                        selected_ids.truncate(selection_limit);
                    }
                    manifest_selection_debug = response;
                }
                Err(error) => {
                    manifest_selection_debug = object(&[("error", string(error))]);
                }
            }
        }

        // TS fallbackSelection: user route gets 1, other routes get 3 (not 5)
        if selected_ids.is_empty() && route != "none" {
            let fallback_limit = if route == "user" { 1 } else { 3 };
            for entry in manifest.iter().take(fallback_limit) {
                if let Some(id) = entry.get("relativePath").and_then(JsValue::as_string)
                    && !selected_ids.contains(id)
                {
                    selected_ids.push(id.clone());
                }
            }
        }
        if input.signal.is_cancelled() {
            return Err("Memory retrieval aborted.".into());
        }

        let records = if selected_ids.is_empty() {
            Vec::new()
        } else {
            // Aligns with TS reasoning-loop.ts:26, 686 RECALL_FILE_MAX_LINES = 200
            let loaded = self
                .inner
                .stores
                .get_memory_records_by_ids(&selected_ids, 200.0)
                .map_err(|error| error.to_string())?;
            match &loaded {
                JsValue::Array(records) => records.clone(),
                _ => Vec::new(),
            }
        };

        let context = if route == "none" {
            String::new()
        } else {
            render_system_context(
                &route,
                user_summary.as_ref(),
                project_meta.as_ref(),
                &records,
            )
        };
        let finished_at = (self.inner.clock)();
        let elapsed_ms = (Instant::now() - start_instant).as_millis() as f64;
        let selected_values = selected_ids
            .iter()
            .cloned()
            .map(JsValue::String)
            .collect::<Vec<_>>();

        let trace_id = format!("recall_trace_{}", uuid::Uuid::new_v4().simple());
        let gate_status = if route == "none" {
            "warning"
        } else {
            "success"
        };
        let mut steps = vec![
            object(&[
                ("stepId", string(format!("{trace_id}:step:1"))),
                ("kind", JsValue::from("recall_start")),
                ("title", JsValue::from("Recall Started")),
                ("status", JsValue::from("info")),
                ("inputSummary", JsValue::from(input.query.as_str())),
                ("outputSummary", string(format!("mode={retrieval_mode}"))),
                (
                    "titleI18n",
                    trace_i18n("trace.step.recall_start", "Recall Started"),
                ),
                (
                    "details",
                    JsValue::Array(vec![kv_detail(
                        "recall-start-inputs",
                        "Recall Inputs",
                        vec![
                            ("query", JsValue::from(input.query.as_str())),
                            ("mode", JsValue::from(retrieval_mode.as_str())),
                            (
                                "recentUserMessages",
                                JsValue::Number(recent_user_count as f64),
                            ),
                            ("workspaceHint", JsValue::from(input.project_root.as_str())),
                        ],
                        Some(trace_i18n("trace.detail.recall_inputs", "Recall Inputs")),
                    )]),
                ),
            ]),
            object(&[
                ("stepId", string(format!("{trace_id}:step:2"))),
                ("kind", JsValue::from("memory_gate")),
                ("title", JsValue::from("Memory Gate")),
                ("status", JsValue::from(gate_status)),
                ("inputSummary", JsValue::from(input.query.as_str())),
                ("outputSummary", string(format!("route={route}"))),
                ("route", string(route.clone())),
                ("promptDebug", gate_debug.clone()),
                (
                    "titleI18n",
                    trace_i18n("trace.step.memory_gate", "Memory Gate"),
                ),
                (
                    "details",
                    JsValue::Array(vec![kv_detail(
                        "gate-route",
                        "Route",
                        vec![
                            ("route", string(route.clone())),
                            (
                                "recentUserMessages",
                                JsValue::Number(recent_user_count as f64),
                            ),
                            ("workspaceHint", JsValue::from(input.project_root.as_str())),
                        ],
                        Some(trace_i18n("trace.detail.route", "Route")),
                    )]),
                ),
            ]),
            object(&[
                ("stepId", string(format!("{trace_id}:step:3"))),
                ("kind", JsValue::from("user_base_loaded")),
                ("title", JsValue::from("User Base Loaded")),
                (
                    "status",
                    JsValue::from(if !needs_user {
                        "skipped"
                    } else if user_summary.is_some() {
                        "success"
                    } else {
                        "warning"
                    }),
                ),
                (
                    "inputSummary",
                    string(if !needs_user {
                        format!("route={route}")
                    } else {
                        "global user profile".to_string()
                    }),
                ),
                (
                    "outputSummary",
                    string(if !needs_user {
                        "Current route does not require user identity background.".to_string()
                    } else if user_summary.is_some() {
                        "Attached compact global user profile.".to_string()
                    } else {
                        "No stable user profile was available.".to_string()
                    }),
                ),
                (
                    "titleI18n",
                    trace_i18n("trace.step.user_base_loaded", "User Base Loaded"),
                ),
                (
                    "details",
                    JsValue::Array(vec![kv_detail(
                        "user-summary",
                        "User Profile",
                        vec![
                            ("route", string(route.clone())),
                            (
                                "required",
                                JsValue::from(if needs_user { "yes" } else { "no" }),
                            ),
                            (
                                "identityBackground",
                                JsValue::Number(if user_summary.is_some() { 1.0 } else { 0.0 }),
                            ),
                        ],
                        Some(trace_i18n("trace.detail.user_profile", "User Profile")),
                    )]),
                ),
            ]),
        ];
        if general {
            let shortlist_idx = steps.len() + 1;
            steps.push(object(&[
                ("stepId", string(format!("{trace_id}:step:{shortlist_idx}"))),
                ("kind", JsValue::from("project_shortlist_built")),
                ("title", JsValue::from("Project Shortlist Built")),
                (
                    "status",
                    JsValue::from(if shortlist_count > 0 {
                        "success"
                    } else {
                        "warning"
                    }),
                ),
                (
                    "inputSummary",
                    string(format!("{catalog_count} readable projects")),
                ),
                (
                    "outputSummary",
                    string(if shortlist_count > 0 {
                        format!(
                            "{shortlist_count} candidate projects are available for General recall."
                        )
                    } else {
                        "No readable projects were available for General recall.".to_string()
                    }),
                ),
                (
                    "titleI18n",
                    trace_i18n(
                        "trace.step.project_shortlist_built",
                        "Project Shortlist Built",
                    ),
                ),
                (
                    "details",
                    JsValue::Array(vec![kv_detail(
                        "project-shortlist-summary",
                        "Project Shortlist Summary",
                        vec![
                            ("catalog", JsValue::Number(catalog_count as f64)),
                            ("shortlist", JsValue::Number(shortlist_count as f64)),
                        ],
                        Some(trace_i18n(
                            "trace.detail.project_shortlist_summary",
                            "Project Shortlist Summary",
                        )),
                    )]),
                ),
            ]));

            let step_idx = steps.len() + 1;
            steps.push(object(&[
                ("stepId", string(format!("{trace_id}:step:{step_idx}"))),
                ("kind", JsValue::from("project_selected")),
                ("title", JsValue::from("Project Selected")),
                (
                    "status",
                    JsValue::from(if selected_project_id.is_some() {
                        "success"
                    } else {
                        "warning"
                    }),
                ),
                (
                    "inputSummary",
                    string(format!("{shortlist_count} candidates")),
                ),
                (
                    "outputSummary",
                    string(
                        selected_project_id
                            .as_deref()
                            .map(|id| format!("{id} selected for General recall."))
                            .unwrap_or_else(|| {
                                "General recall could not resolve a readable project.".to_string()
                            }),
                    ),
                ),
                (
                    "projectId",
                    selected_project_id
                        .as_deref()
                        .map(string)
                        .unwrap_or(JsValue::Null),
                ),
                (
                    "titleI18n",
                    trace_i18n("trace.step.project_selected", "Project Selected"),
                ),
                ("promptDebug", project_selection_debug.clone()),
            ]));
        }

        let scanned_step_idx = steps.len() + 1;
        let scanned_status = if !manifest.is_empty() {
            "success"
        } else if route == "none" {
            "skipped"
        } else {
            "warning"
        };
        let scanned_preview = manifest
            .iter()
            .map(|entry| {
                let updated = entry
                    .get("updatedAt")
                    .and_then(JsValue::as_string)
                    .and_then(|s| s.to_utf8().ok())
                    .unwrap_or_default();
                let ty = entry
                    .get("type")
                    .and_then(JsValue::as_string)
                    .and_then(|s| s.to_utf8().ok())
                    .unwrap_or_default();
                let path = entry
                    .get("relativePath")
                    .and_then(JsValue::as_string)
                    .and_then(|s| s.to_utf8().ok())
                    .unwrap_or_default();
                let desc = entry
                    .get("description")
                    .and_then(JsValue::as_string)
                    .and_then(|s| s.to_utf8().ok())
                    .unwrap_or_default();
                format!("{updated} | {ty} | {path} | {desc}")
            })
            .collect::<Vec<_>>();

        steps.push(object(&[
            (
                "stepId",
                string(format!("{trace_id}:step:{scanned_step_idx}")),
            ),
            ("kind", JsValue::from("manifest_scanned")),
            ("title", JsValue::from("Manifest Scanned")),
            ("status", JsValue::from(scanned_status)),
            ("inputSummary", string(format!("route={route}"))),
            (
                "outputSummary",
                string(if !manifest.is_empty() {
                    format!("{} recall header entries ready.", manifest.len())
                } else {
                    "No matching workspace memory files were available.".to_string()
                }),
            ),
            (
                "titleI18n",
                trace_i18n("trace.step.manifest_scanned", "Manifest Scanned"),
            ),
            (
                "details",
                JsValue::Array(vec![
                    kv_detail(
                        "manifest-scan-summary",
                        "Manifest Scan",
                        vec![
                            ("count", JsValue::Number(manifest.len() as f64)),
                            ("route", string(route.clone())),
                            ("limit", JsValue::Number(200.0)),
                            ("workspaceHint", JsValue::from(input.project_root.as_str())),
                        ],
                        Some(trace_i18n("trace.detail.manifest_scan", "Manifest Scan")),
                    ),
                    list_detail(
                        "manifest-scan-preview",
                        "Sorted Candidates",
                        scanned_preview,
                        Some(trace_i18n(
                            "trace.detail.sorted_candidates",
                            "Sorted Candidates",
                        )),
                    ),
                ]),
            ),
        ]));

        let manifest_step_idx = steps.len() + 1;
        steps.push(object(&[
            (
                "stepId",
                string(format!("{trace_id}:step:{manifest_step_idx}")),
            ),
            ("kind", JsValue::from("manifest_selected")),
            ("title", JsValue::from("Manifest Selected")),
            (
                "status",
                JsValue::from(if !selected_ids.is_empty() {
                    "success"
                } else if !manifest.is_empty() {
                    "warning"
                } else {
                    "skipped"
                }),
            ),
            (
                "inputSummary",
                string(format!("{} entries", manifest.len())),
            ),
            (
                "outputSummary",
                string(format!("{} file ids selected.", selected_ids.len())),
            ),
            ("selectedIds", JsValue::Array(selected_values.clone())),
            (
                "titleI18n",
                trace_i18n("trace.step.manifest_selected", "Manifest Selected"),
            ),
            (
                "details",
                JsValue::Array(vec![
                    list_detail(
                        "manifest-selection-input",
                        "Manifest Candidate IDs",
                        manifest
                            .iter()
                            .filter_map(|e| {
                                e.get("relativePath")
                                    .and_then(JsValue::as_string)
                                    .and_then(|s| s.to_utf8().ok())
                            })
                            .collect(),
                        Some(trace_i18n(
                            "trace.detail.manifest_candidate_ids",
                            "Manifest Candidate IDs",
                        )),
                    ),
                    list_detail(
                        "selected-files",
                        "Selected File IDs",
                        selected_ids
                            .iter()
                            .map(|s| s.to_utf8().unwrap_or_default())
                            .collect(),
                        Some(trace_i18n(
                            "trace.detail.selected_file_ids",
                            "Selected File IDs",
                        )),
                    ),
                ]),
            ),
            ("promptDebug", manifest_selection_debug.clone()),
        ]));

        let files_step_idx = steps.len() + 1;
        steps.push(object(&[
            (
                "stepId",
                string(format!("{trace_id}:step:{files_step_idx}")),
            ),
            ("kind", JsValue::from("files_loaded")),
            ("title", JsValue::from("Files Loaded")),
            (
                "status",
                JsValue::from(if !records.is_empty() {
                    "success"
                } else if !selected_ids.is_empty() {
                    "warning"
                } else {
                    "skipped"
                }),
            ),
            (
                "inputSummary",
                string(format!("{} requested", selected_ids.len())),
            ),
            (
                "outputSummary",
                string(format!("{} files loaded.", records.len())),
            ),
            (
                "titleI18n",
                trace_i18n("trace.step.files_loaded", "Files Loaded"),
            ),
            (
                "details",
                JsValue::Array(vec![
                    list_detail(
                        "requested-files",
                        "Requested IDs",
                        selected_ids
                            .iter()
                            .map(|s| s.to_utf8().unwrap_or_default())
                            .collect(),
                        Some(trace_i18n("trace.detail.requested_ids", "Requested IDs")),
                    ),
                    list_detail(
                        "loaded-files",
                        "Loaded Files",
                        records
                            .iter()
                            .map(|r| {
                                let path = r
                                    .get("relativePath")
                                    .and_then(JsValue::as_string)
                                    .and_then(|s| s.to_utf8().ok())
                                    .unwrap_or_default();
                                let updated = r
                                    .get("updatedAt")
                                    .and_then(JsValue::as_string)
                                    .and_then(|s| s.to_utf8().ok())
                                    .unwrap_or_default();
                                format!("{path} | {updated}")
                            })
                            .collect(),
                        Some(trace_i18n("trace.detail.loaded_files", "Loaded Files")),
                    ),
                ]),
            ),
        ]));

        let context_step_idx = steps.len() + 1;
        let context_status = if !context.is_empty() {
            "success"
        } else if route == "none" {
            "skipped"
        } else {
            "warning"
        };
        steps.push(object(&[
            (
                "stepId",
                string(format!("{trace_id}:step:{context_step_idx}")),
            ),
            ("kind", JsValue::from("context_rendered")),
            ("title", JsValue::from("Context Rendered")),
            ("status", JsValue::from(context_status)),
            ("inputSummary", string(format!("{} records", records.len()))),
            (
                "outputSummary",
                string(if !context.is_empty() {
                    "Memory context prepared.".to_string()
                } else {
                    "No recall context was prepared.".to_string()
                }),
            ),
            ("characters", JsValue::Number(utf16_len(&context) as f64)),
            (
                "titleI18n",
                trace_i18n("trace.step.context_rendered", "Context Rendered"),
            ),
            (
                "details",
                JsValue::Array(vec![kv_detail(
                    "context-rendered-summary",
                    "Context Summary",
                    vec![
                        ("route", string(route.clone())),
                        (
                            "userBaseInjected",
                            JsValue::from(if user_summary.is_some() { "yes" } else { "no" }),
                        ),
                        ("fileCount", JsValue::Number(records.len() as f64)),
                        ("characters", JsValue::Number(utf16_len(&context) as f64)),
                    ],
                    Some(trace_i18n(
                        "trace.detail.context_summary",
                        "Context Summary",
                    )),
                )]),
            ),
        ]));

        let debug_mode = if !context.is_empty() {
            "llm"
        } else if route == "none" {
            "none"
        } else {
            "local_fallback"
        };

        let metadata = object(&[
            (
                "trace",
                object(&[
                    ("traceId", string(trace_id)),
                    ("query", JsValue::from(input.query.as_str())),
                    ("startedAt", string(started_at)),
                    ("finishedAt", string(finished_at)),
                    ("route", string(route.clone())),
                    ("selectedIds", JsValue::Array(selected_values.clone())),
                    ("steps", JsValue::Array(steps)),
                ]),
            ),
            (
                "debug",
                if debug_mode == "none" {
                    object(&[
                        ("mode", JsValue::from("none")),
                        ("elapsedMs", JsValue::Number(elapsed_ms)),
                        ("cacheHit", JsValue::Bool(false)),
                        ("path", JsValue::from("explicit")),
                        ("route", JsValue::from("none")),
                        ("manifestCount", JsValue::Number(0.0)),
                        ("selectedFileIds", JsValue::Array(Vec::new())),
                    ])
                } else {
                    object(&[
                        ("mode", JsValue::from(debug_mode)),
                        ("elapsedMs", JsValue::Number(elapsed_ms)),
                        ("path", JsValue::from(retrieval_mode.as_str())),
                        ("cacheHit", JsValue::Bool(false)),
                        ("route", string(route.clone())),
                        (
                            "resolvedProjectId",
                            selected_project_id
                                .as_deref()
                                .map(string)
                                .unwrap_or(JsValue::Null),
                        ),
                        ("manifestCount", JsValue::Number(manifest.len() as f64)),
                        ("selectedFileIds", JsValue::Array(selected_values)),
                    ])
                },
            ),
        ]);
        let context_is_empty = context.is_empty();
        let result = MemoryRetrieveResult {
            system_context: (!context_is_empty).then_some(context),
            diagnostics: if context_is_empty {
                vec![MemoryDiagnostic {
                    code: "memory_context_empty".into(),
                    severity: "info".into(),
                    message: "EdgeClaw memory returned no relevant context.".into(),
                }]
            } else {
                Vec::new()
            },
            metadata: Some(metadata),
        };
        self.inner
            .recall_cache
            .lock()
            .expect("recall cache mutex poisoned")
            .insert(
                cache_key,
                RecallCacheEntry {
                    expires_at: Instant::now() + Duration::from_secs(30),
                    result: result.clone(),
                },
            );
        Ok(result)
    }

    fn capture_now(&self, input: MemoryCaptureTurnInput) -> Result<(), String> {
        self.ensure_open()?;
        let canonical = canonical_messages(&input.messages, false);
        let normalized = normalize_capture_messages(
            &canonical,
            self.inner.capture_strategy,
            self.inner.include_assistant,
            self.inner.max_message_chars,
        );
        let timestamp = (self.inner.clock)();
        // EdgeClawMemoryProvider swallows capture failures so memory cannot
        // fail the turn. Keep the observable case trace best-effort too.
        if !normalized.is_empty() {
            let is_new = self
                .inner
                .stores
                .storage
                .list_recent_l0(1, 0)
                .ok()
                .and_then(|records| records.into_iter().next())
                .is_none_or(|recent| {
                    recent.session_key != input.session_id
                        || has_new_capture_content(&recent.messages, &normalized)
                });
            if is_new {
                let messages = JsValue::Array(normalized);
                let payload = stringify_json(&messages)
                    .ok()
                    .flatten()
                    .unwrap_or_else(|| "[]".into());
                let session_key = if input.session_id.is_empty() {
                    "session"
                } else {
                    input.session_id.as_str()
                };
                let record = super::L0SessionRecord {
                    l0_index_id: format!(
                        "{session_key}_{}_raw",
                        hash(&format!("{timestamp}:{payload}"))
                    ),
                    session_key: input.session_id.clone(),
                    timestamp: timestamp.clone(),
                    messages,
                    source: self.inner.source.clone(),
                    indexed: false,
                    created_at: timestamp.clone(),
                };
                let _ = self.inner.stores.storage.insert_l0_session(&record);
            }
        }
        if let Some(pending) = self
            .inner
            .pending_retrievals
            .lock()
            .expect("pending retrieval mutex poisoned")
            .remove(&input.session_id)
        {
            let context = pending.result.system_context.unwrap_or_default();
            let metadata = pending
                .result
                .metadata
                .unwrap_or_else(|| JsValue::Object(JsObject::new()));
            let trace = object(&[
                (
                    "caseId",
                    string(format!(
                        "case_trace_{}",
                        hash(&format!(
                            "{}:{}:{}:{}",
                            input.session_id,
                            pending.query,
                            pending.started_at,
                            uuid::Uuid::new_v4().simple()
                        ))
                    )),
                ),
                ("sessionKey", JsValue::from(input.session_id.as_str())),
                ("query", JsValue::from(pending.query.as_str())),
                ("startedAt", JsValue::from(pending.started_at.as_str())),
                ("finishedAt", JsValue::from(timestamp.as_str())),
                (
                    "status",
                    JsValue::from(if input.errored { "error" } else { "completed" }),
                ),
                (
                    "retrieval",
                    object(&[
                        ("injected", JsValue::Bool(!context.is_empty())),
                        ("contextPreview", string(context)),
                        (
                            "intent",
                            metadata
                                .get("debug")
                                .and_then(|debug| debug.get("route"))
                                .cloned()
                                .unwrap_or_else(|| JsValue::from("none")),
                        ),
                        (
                            "trace",
                            metadata.get("trace").cloned().unwrap_or(JsValue::Null),
                        ),
                    ]),
                ),
                ("toolEvents", JsValue::Array(Vec::new())),
                ("assistantReply", string(last_assistant_text(&canonical))),
            ]);
            let _ = self.inner.stores.storage.save_case_trace(&trace, 200);
        }
        Ok(())
    }

    async fn run_maintenance(&self, reason: String) -> Result<(), MemoryProviderError> {
        self.ensure_open().map_err(MemoryProviderError::new)?;
        self.inner
            .maintenance_requested
            .store(true, Ordering::Release);
        let _guard = self.inner.maintenance_gate.lock().await;
        while self
            .inner
            .maintenance_requested
            .swap(false, Ordering::AcqRel)
        {
            self.run_due_cycle(&reason).await?;
        }
        Ok(())
    }

    async fn run_due_cycle(&self, reason: &str) -> Result<(), MemoryProviderError> {
        let settings = self
            .inner
            .stores
            .storage
            .get_indexing_settings(&self.inner.default_indexing_settings)
            .map_err(storage_error)?;
        let pending = self
            .inner
            .stores
            .storage
            .count_pending_dialogue_turns(None)
            .map_err(storage_error)?;
        let now = (self.inner.clock)();
        let index_anchor = if pending > 0 {
            let anchor = self
                .inner
                .stores
                .storage
                .get_pipeline_state("autoIndexAnchorAt")
                .map_err(storage_error)?
                .and_then(|value| value_text(Some(&value)))
                .or(self
                    .inner
                    .stores
                    .storage
                    .get_earliest_pending_timestamp(None)
                    .map_err(storage_error)?);
            if let Some(anchor) = &anchor {
                self.inner
                    .stores
                    .storage
                    .set_pipeline_state("autoIndexAnchorAt", &string(anchor.clone()))
                    .map_err(storage_error)?;
            }
            anchor
        } else {
            self.inner
                .stores
                .storage
                .delete_pipeline_state("autoIndexAnchorAt")
                .map_err(storage_error)?;
            None
        };
        let should_index = pending >= 20
            || (pending > 0
                && index_anchor.as_deref().is_some_and(|anchor| {
                    elapsed_minutes(anchor, &now) >= settings.auto_index_interval_minutes
                }));
        if should_index {
            self.run_index_cycle(reason).await?;
            if self
                .inner
                .stores
                .storage
                .get_earliest_pending_timestamp(None)
                .map_err(storage_error)?
                .is_none()
            {
                self.inner
                    .stores
                    .storage
                    .delete_pipeline_state("autoIndexAnchorAt")
                    .map_err(storage_error)?;
            }
        }

        let last_dream = self
            .inner
            .stores
            .storage
            .get_pipeline_state("lastDreamAt")
            .map_err(storage_error)?
            .and_then(|value| value_text(Some(&value)));
        let mut overview_input = JsObject::new();
        if let Some(last_dream) = &last_dream {
            overview_input.insert("lastDreamAt".into(), string(last_dream.clone()));
        }
        let overview = self
            .inner
            .stores
            .workspace_memory
            .get_overview(&overview_input)
            .map_err(provider_error)?;
        let changed = overview
            .get("changedFilesSinceLastDream")
            .and_then(|value| match value {
                JsValue::Number(value) => Some(*value as i64),
                _ => None,
            })
            .unwrap_or_default();
        let last_indexed = self
            .inner
            .stores
            .storage
            .get_pipeline_state("lastIndexedAt")
            .map_err(storage_error)?
            .and_then(|value| value_text(Some(&value)));
        let dream_anchor = if changed > 0 && last_indexed.is_some() {
            let anchor = self
                .inner
                .stores
                .storage
                .get_pipeline_state("autoDreamAnchorAt")
                .map_err(storage_error)?
                .and_then(|value| value_text(Some(&value)))
                .or(last_indexed);
            if let Some(anchor) = &anchor {
                self.inner
                    .stores
                    .storage
                    .set_pipeline_state("autoDreamAnchorAt", &string(anchor.clone()))
                    .map_err(storage_error)?;
            }
            anchor
        } else {
            self.inner
                .stores
                .storage
                .delete_pipeline_state("autoDreamAnchorAt")
                .map_err(storage_error)?;
            None
        };
        if self.inner.llm.is_some()
            && changed > 0
            && dream_anchor.as_deref().is_some_and(|anchor| {
                elapsed_minutes(anchor, &now) >= settings.auto_dream_interval_minutes
            })
        {
            self.run_dream_cycle("scheduled").await?;
        }
        Ok(())
    }

    async fn run_index_cycle(&self, reason: &str) -> Result<(), MemoryProviderError> {
        let started_at = (self.inner.clock)();
        let sessions = self
            .inner
            .stores
            .storage
            .list_pending_session_keys(30, None)
            .map_err(storage_error)?;
        if sessions.is_empty() {
            return Ok(());
        }
        let mut stored = Vec::new();
        let mut steps = Vec::new();
        let mut had_error = false;
        for session in sessions {
            let rows = self
                .inner
                .stores
                .storage
                .list_unindexed_l0_by_session(&session)
                .map_err(storage_error)?;
            let previous_messages = match rows.first() {
                Some(first) => self
                    .inner
                    .stores
                    .storage
                    .get_latest_l0_before(&session, &first.timestamp, &first.created_at)
                    .map_err(storage_error)?
                    .map(|row| memory_messages(&row.messages))
                    .unwrap_or_default(),
                None => Vec::new(),
            };
            let mut cursor_messages = previous_messages;
            let mut focus_by_row = Vec::with_capacity(rows.len());
            for row in &rows {
                let incoming = memory_messages(&row.messages);
                let (merged, new_messages) = merge_session_messages(&cursor_messages, &incoming);
                let focus_turns = new_messages
                    .into_iter()
                    .filter(|message| {
                        value_text(message.get("role")).as_deref() == Some("user")
                            && value_text(message.get("content"))
                                .is_some_and(|text| !text.is_empty())
                    })
                    .collect::<Vec<_>>();
                focus_by_row.push(focus_turns);
                cursor_messages = merged;
            }
            let messages = cursor_messages;
            let mut session_had_error = false;
            let focus_count = focus_by_row.iter().map(Vec::len).sum::<usize>();
            steps.push(object(&[
                ("kind", JsValue::from("focus_turns_selected")),
                ("sessionKey", JsValue::from(session.as_str())),
                ("count", JsValue::Number(focus_count as f64)),
            ]));
            let mut processed_ids = Vec::new();
            for (row, focus_turns) in rows.iter().zip(focus_by_row) {
                let mut row_had_error = false;
                for focus_turn in focus_turns {
                    let classification_input = object(&[
                        ("timestamp", JsValue::from(row.timestamp.as_str())),
                        ("sessionKey", JsValue::from(session.as_str())),
                        ("focusUserTurn", focus_turn.clone()),
                        ("batchContextMessages", JsValue::Array(messages.clone())),
                        (
                            "currentProjectMeta",
                            self.inner
                                .stores
                                .get_project_meta()
                                .unwrap_or(JsValue::Null),
                        ),
                        ("reason", JsValue::from(reason)),
                    ]);
                    let classification = match &self.inner.llm {
                        Some(llm) => {
                            llm.execute("memory_classify_turn", classification_input)
                                .await
                        }
                        None => Err("Memory LLM is not configured.".into()),
                    };
                    let labels = match classification {
                        Ok(response) => classification_labels(&response),
                        Err(error) => {
                            had_error = true;
                            session_had_error = true;
                            row_had_error = true;
                            steps.push(object(&[
                                ("kind", JsValue::from("classification_error")),
                                ("sessionKey", JsValue::from(session.as_str())),
                                ("message", string(error)),
                            ]));
                            break;
                        }
                    };
                    steps.push(object(&[
                        ("kind", JsValue::from("classification")),
                        ("sessionKey", JsValue::from(session.as_str())),
                        ("labels", JsValue::Array(labels.clone())),
                    ]));
                    let mut created_candidates = Vec::new();
                    for label in labels {
                        let Some(kind) = value_text(label.get("type")) else {
                            continue;
                        };
                        let operation = match kind.as_str() {
                            "user" => "memory_create_user",
                            "project" => "memory_create_project",
                            "feedback" => "memory_create_feedback",
                            _ => continue,
                        };
                        let create_input = object(&[
                            ("timestamp", JsValue::from(row.timestamp.as_str())),
                            ("sessionKey", JsValue::from(session.as_str())),
                            ("focusUserTurn", focus_turn.clone()),
                            ("batchContextMessages", JsValue::Array(messages.clone())),
                            ("classification", label.clone()),
                            (
                                "currentProjectMeta",
                                self.inner
                                    .stores
                                    .get_project_meta()
                                    .unwrap_or(JsValue::Null),
                            ),
                        ]);
                        let response = match &self.inner.llm {
                            Some(llm) => llm.execute(operation, create_input).await,
                            None => Err("Memory LLM is not configured.".into()),
                        };
                        let candidate = match response {
                            Ok(response) => memory_candidate_from_create(
                                &response,
                                &kind,
                                &session,
                                &row.timestamp,
                            ),
                            Err(error) => {
                                had_error = true;
                                session_had_error = true;
                                row_had_error = true;
                                steps.push(object(&[
                                    ("kind", JsValue::from("create_error")),
                                    ("candidateType", string(kind.clone())),
                                    ("message", string(error)),
                                ]));
                                break;
                            }
                        };
                        let Some(mut candidate) = candidate else {
                            continue;
                        };
                        if let Err(error) = self
                            .route_general_index_candidate(
                                &mut candidate,
                                &focus_turn,
                                &messages,
                                &mut steps,
                            )
                            .await
                        {
                            had_error = true;
                            session_had_error = true;
                            row_had_error = true;
                            steps.push(object(&[
                                ("kind", JsValue::from("project_route_error")),
                                ("sessionKey", JsValue::from(session.as_str())),
                                (
                                    "message",
                                    string(String::from_utf16_lossy(error.message.units())),
                                ),
                            ]));
                            break;
                        }
                        created_candidates.push((kind, candidate));
                    }
                    if row_had_error {
                        break;
                    }
                    for (kind, candidate) in created_candidates {
                        let target = if kind == "user" {
                            &*self.inner.stores.global_user_memory
                        } else {
                            &self.inner.stores.workspace_memory
                        };
                        match target.upsert_candidate(&candidate) {
                            Ok(record) => stored.push(record),
                            Err(error) => {
                                had_error = true;
                                session_had_error = true;
                                row_had_error = true;
                                steps.push(object(&[
                                    ("kind", JsValue::from("persist_error")),
                                    ("sessionKey", JsValue::from(session.as_str())),
                                    ("message", string(error.to_string())),
                                ]));
                            }
                        }
                        if row_had_error {
                            break;
                        }
                    }
                    if row_had_error {
                        break;
                    }
                }
                if row_had_error {
                    continue;
                }
                processed_ids.push(row.l0_index_id.clone());
                self.inner
                    .stores
                    .storage
                    .set_pipeline_state("lastIndexedAt", &JsValue::from(row.timestamp.as_str()))
                    .map_err(storage_error)?;
                self.inner
                    .stores
                    .storage
                    .set_pipeline_state(
                        &format!("lastIndexedCursor:{session}"),
                        &JsValue::from(row.timestamp.as_str()),
                    )
                    .map_err(storage_error)?;
            }
            if !processed_ids.is_empty() {
                self.inner
                    .stores
                    .storage
                    .mark_l0_indexed(&processed_ids)
                    .map_err(storage_error)?;
            }
            steps.push(object(&[
                ("kind", JsValue::from("index_finished")),
                ("sessionKey", JsValue::from(session.as_str())),
                ("segments", JsValue::Number(rows.len() as f64)),
                (
                    "status",
                    JsValue::from(if session_had_error {
                        "error"
                    } else {
                        "completed"
                    }),
                ),
            ]));
        }
        let finished_at = (self.inner.clock)();
        let trace = object(&[
            (
                "indexTraceId",
                string(format!("index_trace_{}", uuid::Uuid::new_v4().simple())),
            ),
            ("startedAt", JsValue::from(started_at.as_str())),
            ("finishedAt", JsValue::from(finished_at.as_str())),
            (
                "status",
                JsValue::from(if had_error { "error" } else { "completed" }),
            ),
            ("reason", JsValue::from(reason)),
            ("storedResults", JsValue::Array(stored.clone())),
            ("steps", JsValue::Array(steps)),
        ]);
        self.inner
            .stores
            .storage
            .save_index_trace(&trace, 200)
            .map_err(storage_error)?;
        self.inner
            .stores
            .workspace_memory
            .repair_manifests()
            .map_err(provider_error)?;
        Ok(())
    }

    async fn route_general_index_candidate(
        &self,
        candidate: &mut JsObject,
        focus_turn: &JsValue,
        batch_messages: &[JsValue],
        steps: &mut Vec<JsValue>,
    ) -> Result<(), MemoryProviderError> {
        if value_text(candidate.get("type")).as_deref() == Some("user")
            || self.inner.stores.get_workspace_mode() != "general"
        {
            return Ok(());
        }
        let catalog = self
            .inner
            .stores
            .list_readable_project_catalog()
            .map_err(provider_error)?;
        let local_catalog = match &catalog {
            JsValue::Array(values) => values
                .iter()
                .filter(|entry| {
                    value_text(entry.get("sourceType")).as_deref() != Some("workspace_external")
                })
                .cloned()
                .collect::<Vec<_>>(),
            _ => Vec::new(),
        };
        let query = build_candidate_routing_query(candidate, focus_turn);
        let candidate_preview = build_candidate_memory_preview(candidate);
        let shortlist = build_project_shortlist(&local_catalog, &query, &[]);
        let mut selected_project_id = None;
        let mut decision = "create_new";
        let mut prompt_debug = JsValue::Null;
        if !shortlist.is_empty()
            && let Some(llm) = &self.inner.llm
        {
            let response = llm
                .execute(
                    "memory_index_project",
                    object(&[
                        ("candidate", JsValue::Object(candidate.clone())),
                        ("candidatePreview", string(candidate_preview)),
                        ("focusUserTurn", focus_turn.clone()),
                        (
                            "recentUserMessages",
                            JsValue::Array(recent_user_messages(batch_messages)),
                        ),
                        ("shortlist", JsValue::Array(shortlist.clone())),
                    ]),
                )
                .await
                .map_err(MemoryProviderError::new)?;
            prompt_debug = response.clone();
            if value_text(response.get("decision")).as_deref() == Some("attach_existing") {
                let project_id = value_text(response.get("selected_project_id"))
                    .or_else(|| value_text(response.get("selectedProjectId")));
                if let Some(project_id) = project_id
                    && shortlist.iter().any(|entry| {
                        value_text(entry.get("projectId")).as_deref() == Some(project_id.as_str())
                            && value_text(entry.get("sourceType")).as_deref()
                                != Some("workspace_external")
                    })
                {
                    selected_project_id = Some(project_id);
                    decision = "attach_existing";
                }
            }
        }
        let project_id = match selected_project_id {
            Some(project_id) => project_id,
            None => {
                let name =
                    value_text(candidate.get("name")).unwrap_or_else(|| "Current Project".into());
                let description = value_text(candidate.get("description"))
                    .filter(|value| !value.is_empty())
                    .unwrap_or_else(|| name.clone());
                let meta = self
                    .inner
                    .stores
                    .workspace_memory
                    .upsert_general_project_meta(
                        object(&[
                            ("projectName", string(name)),
                            ("description", string(description)),
                            ("status", JsValue::from("in_progress")),
                            ("sourceKind", JsValue::from("general_local")),
                        ])
                        .as_object()
                        .expect("project metadata input is an object"),
                    )
                    .map_err(provider_error)?;
                value_text(meta.get("projectId")).ok_or_else(|| {
                    MemoryProviderError::new("General project metadata did not return a projectId.")
                })?
            }
        };
        candidate.insert("projectId".into(), string(project_id.clone()));
        steps.push(object(&[
            ("kind", JsValue::from("project_routed")),
            ("decision", JsValue::from(decision)),
            ("projectId", string(project_id)),
            ("promptDebug", prompt_debug),
        ]));
        Ok(())
    }

    async fn merge_general_project_metas(
        &self,
        stage: &super::storage::snapshots::MemoryRepositoryStage,
        llm: &Arc<dyn NativeEdgeClawLlm>,
        changed: &mut Vec<JsValue>,
    ) -> Result<(), MemoryProviderError> {
        if stage.repository.get_workspace_mode() != "general" {
            return Ok(());
        }
        let metas_value = stage
            .repository
            .workspace_memory
            .list_project_metas()
            .map_err(provider_error)?;
        let metas = match &metas_value {
            JsValue::Array(values) => values.clone(),
            _ => Vec::new(),
        };
        if metas.len() < 2 {
            return Ok(());
        }
        let response = match llm
            .execute(
                "memory_dream_general_project_merge_plan",
                object(&[("projectMetas", JsValue::Array(metas.clone()))]),
            )
            .await
        {
            Ok(response) => response,
            Err(_) => return Ok(()),
        };
        for group in validate_general_merge_groups(&response, &metas) {
            if !metas.iter().any(|meta| {
                value_text(meta.get("projectId")).as_deref()
                    == Some(group.keeper_project_id.as_str())
            }) {
                continue;
            }
            for duplicate_id in group.duplicate_project_ids {
                let Some(duplicate) = metas.iter().find(|meta| {
                    value_text(meta.get("projectId")).as_deref() == Some(duplicate_id.as_str())
                }) else {
                    continue;
                };
                let reassigned = stage
                    .repository
                    .workspace_memory
                    .reassign_project_entries(
                        object(&[
                            ("fromProjectId", string(duplicate_id.clone())),
                            ("toProjectId", string(group.keeper_project_id.clone())),
                        ])
                        .as_object()
                        .expect("reassign input is an object"),
                    )
                    .map_err(provider_error)?;
                if has_mutated_ids(&reassigned) {
                    changed.push(reassigned);
                }
                let Some(relative_path) = duplicate
                    .get("relativePath")
                    .and_then(JsValue::as_string)
                    .cloned()
                else {
                    continue;
                };
                let deleted = stage
                    .repository
                    .workspace_memory
                    .delete_entries(
                        object(&[(
                            "relativePaths",
                            JsValue::Array(vec![JsValue::String(relative_path)]),
                        )])
                        .as_object()
                        .expect("delete input is an object"),
                    )
                    .map_err(provider_error)?;
                if has_mutated_ids(&deleted) {
                    changed.push(deleted);
                }
            }
        }
        Ok(())
    }

    async fn review_project_meta(
        &self,
        stage: &super::storage::snapshots::MemoryRepositoryStage,
        llm: &Arc<dyn NativeEdgeClawLlm>,
        changed: &mut Vec<JsValue>,
        meta: JsValue,
    ) -> Result<(), MemoryProviderError> {
        let catalog_value = stage
            .repository
            .list_readable_project_catalog()
            .map_err(provider_error)?;
        let catalog = match &catalog_value {
            JsValue::Array(values) => values.clone(),
            _ => Vec::new(),
        };
        let Some(project_id) = value_text(meta.get("projectId")) else {
            return Ok(());
        };
        let logical_id = catalog
            .iter()
            .find(|entry| {
                value_text(entry.get("projectId")).as_deref() == Some(project_id.as_str())
            })
            .and_then(|entry| value_text(entry.get("logicalProjectId")))
            .unwrap_or_else(|| project_id.clone());
        let entries_value = stage
            .repository
            .list_readable_project_entries(&logical_id, &JsObject::new())
            .map_err(provider_error)?;
        let entries = match &entries_value {
            JsValue::Array(values) => values.clone(),
            _ => Vec::new(),
        };
        let selected_entries = recent_project_meta_entries(&entries);
        if selected_entries.is_empty() {
            return Ok(());
        }
        let ids = selected_entries
            .iter()
            .filter_map(|entry| entry.get("relativePath"))
            .filter_map(JsValue::as_string)
            .cloned()
            .collect::<Vec<_>>();
        let records_value = stage
            .repository
            .get_memory_records_by_ids(&ids, 5000.0)
            .map_err(provider_error)?;
        let records = match &records_value {
            JsValue::Array(values) => values.clone(),
            _ => Vec::new(),
        };
        let response = llm
            .execute(
                "memory_dream_project_meta_review",
                object(&[
                    ("currentMeta", meta.clone()),
                    ("recentRecords", JsValue::Array(records)),
                ]),
            )
            .await
            .map_err(MemoryProviderError::new)?;
        if response.get("should_update").and_then(JsValue::as_bool) != Some(true)
            && response.get("shouldUpdate").and_then(JsValue::as_bool) != Some(true)
        {
            return Ok(());
        }
        let Some(next) = response
            .get("project_meta")
            .or_else(|| response.get("projectMeta"))
            .and_then(JsValue::as_object)
        else {
            return Ok(());
        };
        let project_name = value_text(next.get("project_name"))
            .or_else(|| value_text(next.get("projectName")))
            .unwrap_or_else(|| value_text(meta.get("projectName")).unwrap_or_default());
        let description = value_text(next.get("description"))
            .unwrap_or_else(|| value_text(meta.get("description")).unwrap_or_default());
        let status = value_text(next.get("status"))
            .unwrap_or_else(|| value_text(meta.get("status")).unwrap_or_default());
        if project_name.is_empty() || description.is_empty() || status.is_empty() {
            return Ok(());
        }
        if value_text(meta.get("projectName")).as_deref() == Some(project_name.as_str())
            && value_text(meta.get("description")).as_deref() == Some(description.as_str())
            && value_text(meta.get("status")).as_deref() == Some(status.as_str())
        {
            return Ok(());
        }
        let mut input = JsObject::new();
        input.insert("projectId".into(), string(project_id));
        input.insert("projectName".into(), string(project_name));
        input.insert("description".into(), string(description));
        input.insert("status".into(), string(status));
        input.insert("dreamUpdatedAt".into(), string((self.inner.clock)()));
        for key in ["sourceKind", "sourceWorkspacePath", "sourceProjectId"] {
            if let Some(value) = meta.get(key) {
                input.insert(key.into(), value.clone());
            }
        }
        changed.push(
            stage
                .repository
                .workspace_memory
                .upsert_project_meta(&input)
                .map_err(provider_error)?,
        );
        Ok(())
    }

    async fn run_dream_category(
        &self,
        stage: &super::storage::snapshots::MemoryRepositoryStage,
        llm: &Arc<dyn NativeEdgeClawLlm>,
        trigger: &str,
        kind: &str,
        records: Vec<JsValue>,
        project_meta: Option<&JsValue>,
        changed: &mut Vec<JsValue>,
    ) -> Result<Option<String>, MemoryProviderError> {
        let headers = dream_record_headers(&records);
        if headers.len() < 2 {
            return Ok(None);
        }
        let plan = llm
            .execute(
                "memory_dream_cluster_plan",
                object(&[
                    ("trigger", JsValue::from(trigger)),
                    ("kind", JsValue::from(kind)),
                    ("headers", JsValue::Array(headers)),
                    (
                        "projectMeta",
                        project_meta.cloned().unwrap_or(JsValue::Null),
                    ),
                ]),
            )
            .await
            .map_err(MemoryProviderError::new)?;
        let (clusters, _dropped_warnings) = validate_dream_clusters(&plan, kind, &records);
        let summary = value_text(plan.get("summary"));
        for cluster in clusters {
            let cluster_ids = cluster.member_ids.iter().cloned().collect::<HashSet<_>>();
            let cluster_records = records
                .iter()
                .filter(|record| {
                    record
                        .get("relativePath")
                        .and_then(JsValue::as_string)
                        .is_some_and(|id| cluster_ids.contains(id))
                })
                .cloned()
                .collect::<Vec<_>>();
            let project_id = project_meta
                .and_then(|meta| value_text(meta.get("projectId")))
                .or_else(|| {
                    cluster_records
                        .first()
                        .and_then(|record| value_text(record.get("projectId")))
                });
            let operation = if kind == "project" {
                "memory_dream_project_rewrite"
            } else {
                "memory_dream_feedback_rewrite"
            };
            let rewrite = llm
                .execute(
                    operation,
                    object(&[
                        ("trigger", JsValue::from(trigger)),
                        ("clusterReason", string(cluster.reason)),
                        ("records", JsValue::Array(cluster_records)),
                        (
                            "projectMeta",
                            project_meta.cloned().unwrap_or(JsValue::Null),
                        ),
                    ]),
                )
                .await
                .map_err(MemoryProviderError::new)?;
            apply_dream_rewrite(
                stage,
                &rewrite,
                kind,
                project_id.as_deref(),
                &cluster_ids,
                changed,
            )?;
        }
        Ok(summary)
    }

    async fn run_dream_cycle(&self, trigger: &str) -> Result<(), MemoryProviderError> {
        let started_at = (self.inner.clock)();
        let result = self.run_dream_cycle_inner(trigger, &started_at).await;
        if let Err(err) = &result {
            let message = String::from_utf16_lossy(err.message.units());
            let trace = dream_trace(&started_at, trigger, "error", &message, Vec::new());
            let _ = self.inner.stores.storage.save_dream_trace(&trace, 200);
        }
        result
    }

    async fn run_dream_cycle_inner(
        &self,
        trigger: &str,
        started_at: &str,
    ) -> Result<(), MemoryProviderError> {
        let Some(llm) = &self.inner.llm else {
            return Ok(());
        };
        self.run_index_cycle(if trigger == "manual" {
            "manual_dream_prep"
        } else {
            "scheduled_dream_prep"
        })
        .await?;
        let stage = DreamStageGuard(Some(
            self.inner
                .stores
                .create_dream_stage("dream")
                .map_err(provider_error)?,
        ));
        let initial_entries = stage
            .get()
            .repository
            .list_memory_entries(5000)
            .map_err(provider_error)?;
        let initial_records = dream_records_for_entries(stage.get(), &initial_entries)?;
        let user_records = initial_records
            .iter()
            .filter(|record| {
                value_text(record.get("type")).as_deref() == Some("user")
                    && value_text(record.get("relativePath"))
                        .is_some_and(|path| is_dream_user_note_path(&path))
            })
            .cloned()
            .collect::<Vec<_>>();
        let user_records = select_dream_user_note_window(&user_records);
        let user_summary = stage
            .get()
            .repository
            .get_user_summary()
            .unwrap_or(JsValue::Null);
        let is_general = stage.get().repository.get_workspace_mode() == "general";
        let initial_general_metas = dream_project_metas(stage.get())?;
        let has_workspace_files = initial_records.iter().any(|record| {
            matches!(
                value_text(record.get("type")).as_deref(),
                Some("project" | "feedback")
            )
        });
        let has_user_profile = user_summary
            .get("files")
            .and_then(|value| match value {
                JsValue::Array(files) => Some(!files.is_empty()),
                _ => None,
            })
            .unwrap_or(false);
        let empty = !has_workspace_files
            && !has_user_profile
            && user_records.is_empty()
            && (!is_general || initial_general_metas.is_empty());
        let mut changed = Vec::new();
        let mut summaries = Vec::new();

        if !empty {
            self.merge_general_project_metas(stage.get(), llm, &mut changed)
                .await?;
            if is_general {
                for meta in dream_project_metas(stage.get())? {
                    let records = dream_records_for_project(stage.get(), &meta)?;
                    for kind in ["project", "feedback"] {
                        let category_records = records
                            .iter()
                            .filter(|record| {
                                value_text(record.get("type")).as_deref() == Some(kind)
                            })
                            .cloned()
                            .collect::<Vec<_>>();
                        if let Some(summary) = self
                            .run_dream_category(
                                stage.get(),
                                llm,
                                trigger,
                                kind,
                                category_records,
                                Some(&meta),
                                &mut changed,
                            )
                            .await?
                        {
                            summaries.push(summary);
                        }
                    }
                    self.review_project_meta(stage.get(), llm, &mut changed, meta)
                        .await?;
                }
            } else {
                let project_meta = stage
                    .get()
                    .repository
                    .get_project_meta()
                    .unwrap_or(JsValue::Null);
                for kind in ["project", "feedback"] {
                    let category_records = initial_records
                        .iter()
                        .filter(|record| value_text(record.get("type")).as_deref() == Some(kind))
                        .cloned()
                        .collect::<Vec<_>>();
                    if let Some(summary) = self
                        .run_dream_category(
                            stage.get(),
                            llm,
                            trigger,
                            kind,
                            category_records,
                            project_meta.as_object().map(|_| &project_meta),
                            &mut changed,
                        )
                        .await?
                    {
                        summaries.push(summary);
                    }
                }
                if project_meta.as_object().is_some() {
                    self.review_project_meta(stage.get(), llm, &mut changed, project_meta)
                        .await?;
                }
            }
        }

        if !empty && !user_records.is_empty() {
            let existing_profile = stage
                .get()
                .repository
                .get_user_summary()
                .unwrap_or(JsValue::Null);
            let merge = llm
                .execute(
                    "memory_dream_user_profile_merge",
                    object(&[
                        ("records", JsValue::Array(user_records.clone())),
                        ("existingProfile", existing_profile.clone()),
                    ]),
                )
                .await;
            if let Ok(merge) = merge
                && merge.get("skip").and_then(JsValue::as_bool) != Some(true)
            {
                let candidate_value = merge.get("candidate").unwrap_or(&merge);
                if let Some(candidate) = dream_user_profile_candidate(candidate_value) {
                    let next_profile = value_text(candidate.get("profile")).unwrap_or_default();
                    if user_profile_body(&existing_profile) != next_profile {
                        changed.push(
                            stage
                                .get()
                                .repository
                                .global_user_memory
                                .upsert_user_profile(&candidate)
                                .map_err(provider_error)?,
                        );
                        let mut absorbed_note_ids = user_records
                            .iter()
                            .filter_map(|record| record.get("relativePath"))
                            .filter_map(JsValue::as_string)
                            .cloned()
                            .collect::<Vec<_>>();
                        absorbed_note_ids.sort_by(|left, right| left.units().cmp(right.units()));
                        absorbed_note_ids.dedup();
                        if !absorbed_note_ids.is_empty() {
                            let deleted = stage
                                .get()
                                .repository
                                .delete_memory_entries(&absorbed_note_ids)
                                .map_err(provider_error)?;
                            if has_mutated_ids(&deleted) {
                                changed.push(deleted);
                            }
                        }
                    }
                }
            }
        }
        let summary = if empty {
            "No file-based memory exists yet, so Dream had nothing to organize.".to_owned()
        } else if summaries.is_empty() {
            "Memory dream completed.".to_owned()
        } else {
            summaries.join(" ")
        };
        let finished_at = (self.inner.clock)();
        let is_no_op = changed.is_empty();
        if is_no_op {
            stage.dispose().map_err(provider_error)?;
        } else {
            let staged_snapshot = stage
                .get()
                .repository
                .capture_current_memory_snapshot()
                .map_err(provider_error)?;
            stage.get().repository.close().map_err(provider_error)?;
            let metadata = object(&[
                ("sourceAction", JsValue::from("dream")),
                ("capturedAt", string(finished_at.clone())),
                ("trigger", JsValue::from(trigger)),
                ("summary", string(summary.clone())),
            ]);
            self.inner
                .stores
                .install_last_dream_snapshot(&stage.get().snapshot, &metadata)
                .map_err(provider_error)?;
            self.inner
                .stores
                .replace_live_roots_with_stage(stage.get(), &stage.get().snapshot)
                .map_err(provider_error)?;
            let _ = staged_snapshot;
            stage.dispose().map_err(provider_error)?;
        }
        self.inner
            .stores
            .storage
            .set_pipeline_state("lastDreamAt", &string(finished_at.clone()))
            .map_err(storage_error)?;
        self.inner
            .stores
            .storage
            .set_pipeline_state("lastDreamStatus", &JsValue::from("success"))
            .map_err(storage_error)?;
        self.inner
            .stores
            .storage
            .set_pipeline_state("lastDreamSummary", &string(summary.clone()))
            .map_err(storage_error)?;
        let trace = dream_trace(&started_at, trigger, "completed", &summary, changed);
        self.inner
            .stores
            .storage
            .save_dream_trace(&trace, 200)
            .map_err(storage_error)?;
        self.inner
            .stores
            .storage
            .delete_pipeline_state("autoDreamAnchorAt")
            .map_err(storage_error)?;
        self.inner
            .stores
            .workspace_memory
            .repair_manifests()
            .map_err(provider_error)?;
        Ok(())
    }
}

fn dream_records_for_entries(
    stage: &super::storage::snapshots::MemoryRepositoryStage,
    entries: &[JsValue],
) -> Result<Vec<JsValue>, MemoryProviderError> {
    let ids = entries
        .iter()
        .filter_map(|entry| entry.get("relativePath"))
        .filter_map(JsValue::as_string)
        .cloned()
        .collect::<Vec<_>>();
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    Ok(
        match &stage
            .repository
            .get_memory_records_by_ids(&ids, 5000.0)
            .map_err(provider_error)?
        {
            JsValue::Array(records) => records.clone(),
            _ => Vec::new(),
        },
    )
}

fn dream_project_metas(
    stage: &super::storage::snapshots::MemoryRepositoryStage,
) -> Result<Vec<JsValue>, MemoryProviderError> {
    Ok(
        match &stage
            .repository
            .workspace_memory
            .list_project_metas()
            .map_err(provider_error)?
        {
            JsValue::Array(metas) => metas.clone(),
            _ => Vec::new(),
        },
    )
}

fn dream_records_for_project(
    stage: &super::storage::snapshots::MemoryRepositoryStage,
    meta: &JsValue,
) -> Result<Vec<JsValue>, MemoryProviderError> {
    let Some(project_id) = value_text(meta.get("projectId")) else {
        return Ok(Vec::new());
    };
    let catalog = match &stage
        .repository
        .list_readable_project_catalog()
        .map_err(provider_error)?
    {
        JsValue::Array(values) => values.clone(),
        _ => Vec::new(),
    };
    let logical_id = catalog
        .iter()
        .find(|entry| value_text(entry.get("projectId")).as_deref() == Some(project_id.as_str()))
        .and_then(|entry| value_text(entry.get("logicalProjectId")))
        .unwrap_or(project_id);
    let entries = match &stage
        .repository
        .list_readable_project_entries(&logical_id, &JsObject::new())
        .map_err(provider_error)?
    {
        JsValue::Array(entries) => entries.clone(),
        _ => Vec::new(),
    };
    dream_records_for_entries(stage, &entries)
}

fn dream_record_headers(records: &[JsValue]) -> Vec<JsValue> {
    const HEADER_SCAN_LIMIT: usize = 200;
    let mut records = records.to_vec();
    records.sort_by(|left, right| {
        value_text(right.get("updatedAt"))
            .unwrap_or_default()
            .cmp(&value_text(left.get("updatedAt")).unwrap_or_default())
            .then_with(|| {
                value_text(left.get("relativePath"))
                    .unwrap_or_default()
                    .cmp(&value_text(right.get("relativePath")).unwrap_or_default())
            })
    });
    records.truncate(HEADER_SCAN_LIMIT);
    records
        .into_iter()
        .map(|record| {
            let mut header = JsObject::new();
            for key in [
                "relativePath",
                "name",
                "description",
                "updatedAt",
                "capturedAt",
            ] {
                if let Some(value) = record.get(key) {
                    header.insert(key.into(), value.clone());
                }
            }
            JsValue::Object(header)
        })
        .collect()
}

fn is_dream_user_note_path(path: &str) -> bool {
    let normalized = path.replace('\\', "/");
    let relative = normalized.strip_prefix("global/").unwrap_or(&normalized);
    relative.starts_with("UserIdentityNotes/")
}

fn select_dream_user_note_window(records: &[JsValue]) -> Vec<JsValue> {
    const MAX_FILES: usize = 200;
    const CHAR_BUDGET: usize = 120_000;
    let mut sorted = records.to_vec();
    sorted.sort_by(|left, right| {
        value_text(right.get("updatedAt"))
            .unwrap_or_default()
            .cmp(&value_text(left.get("updatedAt")).unwrap_or_default())
            .then_with(|| {
                value_text(left.get("relativePath"))
                    .unwrap_or_default()
                    .cmp(&value_text(right.get("relativePath")).unwrap_or_default())
            })
    });
    let mut selected = Vec::new();
    let mut chars = 0usize;
    for record in sorted {
        if selected.len() >= MAX_FILES {
            break;
        }
        let content = value_text(record.get("content"))
            .or_else(|| value_text(record.get("body")))
            .unwrap_or_default();
        let next = chars.saturating_add(utf16_len(&content));
        if next > CHAR_BUDGET {
            break;
        }
        chars = next;
        selected.push(record);
    }
    selected
}

fn validate_dream_clusters(
    plan: &JsValue,
    kind: &str,
    records: &[JsValue],
) -> (Vec<DreamCluster>, Vec<String>) {
    const HEADER_SCAN_LIMIT: usize = 200;
    const CLUSTER_MAX_FILES: usize = 8;
    let mut headers = records.to_vec();
    headers.sort_by(|left, right| {
        value_text(right.get("updatedAt"))
            .unwrap_or_default()
            .cmp(&value_text(left.get("updatedAt")).unwrap_or_default())
            .then_with(|| {
                value_text(left.get("relativePath"))
                    .unwrap_or_default()
                    .cmp(&value_text(right.get("relativePath")).unwrap_or_default())
            })
    });
    headers.truncate(HEADER_SCAN_LIMIT);
    let allowed = headers
        .iter()
        .filter_map(|record| record.get("relativePath")?.as_string().cloned())
        .collect::<HashSet<_>>();
    let snake = format!("{kind}_clusters");
    let camel = format!("{kind}Clusters");
    let raw_clusters = plan
        .get(&snake)
        .or_else(|| plan.get(&camel))
        .or_else(|| plan.get("clusters"))
        .and_then(|value| match value {
            JsValue::Array(values) => Some(values.as_slice()),
            _ => None,
        })
        .unwrap_or_default();
    let mut accepted = Vec::new();
    let mut warnings = Vec::new();
    let mut used = HashSet::new();
    for (index, cluster) in raw_clusters.iter().enumerate() {
        let members = cluster
            .get("member_relative_paths")
            .or_else(|| cluster.get("memberRelativePaths"))
            .and_then(|value| match value {
                JsValue::Array(values) => Some(values.as_slice()),
                _ => None,
            })
            .unwrap_or_default();
        let mut unique = Vec::new();
        for id in members.iter().filter_map(JsValue::as_string) {
            if allowed.contains(id) && !unique.contains(id) {
                unique.push(id.clone());
            }
        }
        let label = index + 1;
        if unique.len() < 2 {
            warnings.push(format!(
                "Dropped {kind} cluster {label}: fewer than two valid files."
            ));
            continue;
        }
        if unique.len() > CLUSTER_MAX_FILES {
            warnings.push(format!(
                "Dropped {kind} cluster {label}: exceeded the {CLUSTER_MAX_FILES}-file limit."
            ));
            continue;
        }
        if unique.iter().any(|id| used.contains(id)) {
            warnings.push(format!("Dropped overlapping {kind} cluster {label}."));
            continue;
        }
        let mut cluster_projects = HashSet::new();
        for id in &unique {
            if let Some(record) = records
                .iter()
                .find(|r| r.get("relativePath").and_then(JsValue::as_string) == Some(id))
            {
                if let Some(proj) = value_text(record.get("projectId")).filter(|p| !p.is_empty()) {
                    cluster_projects.insert(proj);
                }
            }
        }
        if cluster_projects.len() > 1 {
            warnings.push(format!("Dropped cross-project {kind} cluster {label}."));
            continue;
        }
        let reason = value_text(cluster.get("reason")).unwrap_or_default();
        if generic_same_project_reason(&reason) {
            warnings.push(format!(
                "Dropped low-quality {kind} cluster {label}: reason only described project membership."
            ));
            continue;
        }
        used.extend(unique.iter().cloned());
        accepted.push(DreamCluster {
            member_ids: unique,
            reason,
        });
    }
    (accepted, warnings)
}

fn generic_same_project_reason(reason: &str) -> bool {
    let normalized = reason.split_whitespace().collect::<Vec<_>>().join(" ");
    if normalized.is_empty() {
        return false;
    }
    let lower = normalized.to_lowercase();
    let same_project = [
        "same current project",
        "same project",
        "same workspace",
        "belong to the same project",
        "同一个项目",
        "同一项目",
        "属于当前项目",
        "同一个工作区",
    ]
    .iter()
    .any(|needle| lower.contains(needle));
    let semantic = [
        "overlap",
        "duplicate",
        "redundan",
        "conflict",
        "inconsistent",
        "merge",
        "consolidat",
        "same rule",
        "same constraint",
        "重复",
        "冗余",
        "冲突",
        "重叠",
        "可合并",
        "可统一",
        "语义重合",
        "事实重合",
    ]
    .iter()
    .any(|needle| lower.contains(needle));
    same_project && !semantic
}

fn validate_general_merge_groups(response: &JsValue, metas: &[JsValue]) -> Vec<GeneralMergeGroup> {
    let known = metas
        .iter()
        .filter_map(|meta| value_text(meta.get("projectId")))
        .collect::<HashSet<_>>();
    let groups = response
        .get("merge_groups")
        .or_else(|| response.get("mergeGroups"))
        .and_then(|value| match value {
            JsValue::Array(values) => Some(values.as_slice()),
            _ => None,
        })
        .unwrap_or_default();
    let mut accepted = Vec::new();
    let mut used = HashSet::new();
    for group in groups {
        let Some(keeper) = value_text(group.get("keeper_project_id"))
            .or_else(|| value_text(group.get("keeperProjectId")))
        else {
            continue;
        };
        if !known.contains(&keeper) || used.contains(&keeper) {
            continue;
        }
        let duplicate_values = group
            .get("duplicate_project_ids")
            .or_else(|| group.get("duplicateProjectIds"))
            .and_then(|value| match value {
                JsValue::Array(values) => Some(values.as_slice()),
                _ => None,
            })
            .unwrap_or_default();
        let mut duplicates = Vec::new();
        for duplicate in duplicate_values.iter().filter_map(JsValue::as_string) {
            let duplicate = String::from_utf16_lossy(duplicate.units());
            if !duplicate.is_empty() && !duplicates.contains(&duplicate) {
                duplicates.push(duplicate);
            }
        }
        if duplicates.is_empty()
            || duplicates
                .iter()
                .any(|id| id == &keeper || !known.contains(id) || used.contains(id))
        {
            continue;
        }
        used.insert(keeper.clone());
        used.extend(duplicates.iter().cloned());
        accepted.push(GeneralMergeGroup {
            keeper_project_id: keeper,
            duplicate_project_ids: duplicates,
        });
    }
    accepted
}

fn recent_project_meta_entries(entries: &[JsValue]) -> Vec<JsValue> {
    let recent = |kind: &str| {
        let mut values = entries
            .iter()
            .filter(|entry| value_text(entry.get("type")).as_deref() == Some(kind))
            .cloned()
            .collect::<Vec<_>>();
        values.sort_by(|left, right| {
            value_text(right.get("updatedAt"))
                .unwrap_or_default()
                .cmp(&value_text(left.get("updatedAt")).unwrap_or_default())
                .then_with(|| {
                    value_text(left.get("relativePath"))
                        .unwrap_or_default()
                        .cmp(&value_text(right.get("relativePath")).unwrap_or_default())
                })
        });
        values.truncate(5);
        values
    };
    let mut values = recent("project");
    values.extend(recent("feedback"));
    values
}

fn has_mutated_ids(value: &JsValue) -> bool {
    value
        .get("mutatedIds")
        .and_then(|value| match value {
            JsValue::Array(values) => Some(!values.is_empty()),
            _ => None,
        })
        .unwrap_or(false)
}

fn user_profile_body(summary: &JsValue) -> String {
    summary
        .get("files")
        .and_then(|value| match value {
            JsValue::Array(values) => values.first(),
            _ => None,
        })
        .and_then(|file| value_text(file.get("content")))
        .unwrap_or_default()
        .trim()
        .to_owned()
}

fn apply_dream_rewrite(
    stage: &super::storage::snapshots::MemoryRepositoryStage,
    response: &JsValue,
    kind: &str,
    project_id: Option<&str>,
    allowed: &HashSet<JsString>,
    changed: &mut Vec<JsValue>,
) -> Result<(), MemoryProviderError> {
    let file = response
        .get("file")
        .or_else(|| {
            response.get("candidates").and_then(|c| match c {
                JsValue::Array(arr) => arr.first(),
                _ => None,
            })
        })
        .or_else(|| response.get("candidate"))
        .and_then(JsValue::as_object);
    let Some(file) = file else {
        return Ok(());
    };
    let mut candidate = file.clone();
    let name = value_text(candidate.get("name")).unwrap_or_default();
    let description = value_text(candidate.get("description")).unwrap_or_default();
    let body = value_text(candidate.get("markdown"))
        .or_else(|| value_text(candidate.get("body")))
        .unwrap_or_default();
    if name.is_empty() || description.is_empty() || body.is_empty() {
        return Ok(());
    }
    let mut member_ids = allowed.iter().cloned().collect::<Vec<_>>();
    member_ids.sort_by(|left, right| left.units().cmp(right.units()));
    let deleted = stage
        .repository
        .delete_memory_entries(&member_ids)
        .map_err(provider_error)?;
    if has_mutated_ids(&deleted) {
        changed.push(deleted);
    }
    candidate.insert("type".into(), JsValue::from(kind));
    candidate.insert("scope".into(), JsValue::from("project"));
    candidate.insert("body".into(), string(body));
    if let Some(project_id) = project_id.filter(|value| !value.is_empty()) {
        candidate.insert("projectId".into(), string(project_id));
    }
    changed.push(
        stage
            .repository
            .workspace_memory
            .upsert_candidate(&candidate)
            .map_err(provider_error)?,
    );
    Ok(())
}

fn dream_user_profile_candidate(value: &JsValue) -> Option<JsObject> {
    let mut candidate = value.as_object()?.clone();
    let name = value_text(candidate.get("name")).unwrap_or_else(|| "user-profile".into());
    let description = value_text(candidate.get("description"))
        .unwrap_or_else(|| "Stable user identity background".into());
    let profile = value_text(candidate.get("profile"))
        .or_else(|| value_text(candidate.get("body")))
        .unwrap_or_default();
    if profile.is_empty() {
        return None;
    }
    candidate.insert("type".into(), JsValue::from("user"));
    candidate.insert("scope".into(), JsValue::from("global"));
    candidate.insert("name".into(), string(name));
    candidate.insert("description".into(), string(description));
    candidate.insert("profile".into(), string(profile));
    Some(candidate)
}

fn classification_labels(response: &JsValue) -> Vec<JsValue> {
    let should_store = response
        .get("should_store")
        .or_else(|| response.get("shouldStore"))
        .and_then(JsValue::as_bool)
        .unwrap_or(false);
    if !should_store {
        return Vec::new();
    }
    let mut seen = HashSet::new();
    match response.get("labels") {
        Some(JsValue::Array(labels)) => labels
            .iter()
            .filter(|label| {
                value_text(label.get("type")).is_some_and(|kind| {
                    matches!(kind.as_str(), "user" | "project" | "feedback") && seen.insert(kind)
                })
            })
            .cloned()
            .collect(),
        _ => Vec::new(),
    }
}

fn memory_candidate_from_create(
    response: &JsValue,
    kind: &str,
    session: &str,
    timestamp: &str,
) -> Option<JsObject> {
    if response.get("skip").and_then(JsValue::as_bool) == Some(true) {
        return None;
    }
    let payload = response.get("candidate").unwrap_or(response);
    let mut candidate = payload.as_object()?.clone();
    let name = value_text(candidate.get("name")).unwrap_or_default();
    let description = value_text(candidate.get("description")).unwrap_or_default();
    let body = value_text(candidate.get("body"))
        .or_else(|| value_text(candidate.get("markdown")))
        .unwrap_or_default();
    if name.is_empty() || description.is_empty() || body.is_empty() {
        return None;
    }
    candidate.insert("type".into(), JsValue::from(kind));
    candidate.insert(
        "scope".into(),
        JsValue::from(if kind == "user" { "global" } else { "project" }),
    );
    candidate.insert("body".into(), string(body));
    candidate.insert("capturedAt".into(), JsValue::from(timestamp));
    candidate.insert("sourceSessionKey".into(), JsValue::from(session));
    Some(candidate)
}

fn recall_cache_key(
    stores: &EdgeClawFileStores,
    input: &MemoryRetrieveInput,
    planning_manifest: &[JsValue],
) -> Result<String, String> {
    let user_summary = stores
        .get_user_summary()
        .map_err(|error| error.to_string())?;
    let catalog = if stores.get_workspace_mode() == "general" {
        stores
            .list_readable_project_catalog()
            .map_err(|error| error.to_string())?
    } else {
        JsValue::Array(Vec::new())
    };
    let snapshot = stringify_json(&object(&[
        ("manifest", JsValue::Array(planning_manifest.to_vec())),
        ("userSummary", user_summary),
        ("projectCatalog", catalog),
    ]))
    .map_err(|error| error.to_string())?
    .unwrap_or_else(|| "null".into());
    let recent = recent_user_messages(&canonical_messages(&input.recent_messages, true))
        .iter()
        .filter_map(|message| value_text(message.get("content")))
        .collect::<Vec<_>>()
        .join("\n");
    Ok(format!(
        "auto\u{0}{}\u{0}{recent}\u{0}{snapshot}",
        input
            .query
            .to_lowercase()
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
    ))
}

fn mark_recall_cache_hit(mut result: MemoryRetrieveResult) -> MemoryRetrieveResult {
    if let Some(JsValue::Object(metadata)) = result.metadata.as_mut() {
        let mut debug = metadata
            .get("debug")
            .and_then(JsValue::as_object)
            .cloned()
            .unwrap_or_default();
        debug.insert("cacheHit".into(), JsValue::Bool(true));
        debug.insert("elapsedMs".into(), JsValue::Number(0.0));
        metadata.insert("debug".into(), JsValue::Object(debug));
    }
    result
}

fn recent_user_messages(messages: &[JsValue]) -> Vec<JsValue> {
    let mut users = messages
        .iter()
        .filter(|message| value_text(message.get("role")).as_deref() == Some("user"))
        .cloned()
        .collect::<Vec<_>>();
    if users.len() > 4 {
        users.drain(..users.len() - 4);
    }
    users
}

fn build_project_shortlist(
    catalog: &[JsValue],
    query: &str,
    recent_messages: &[JsValue],
) -> Vec<JsValue> {
    let query_lower = query.to_lowercase();
    let recent = recent_user_messages(recent_messages)
        .iter()
        .filter_map(|message| value_text(message.get("content")))
        .collect::<Vec<_>>()
        .join(" ");
    let mut tokens = tokenize_search_text(query);
    for token in tokenize_search_text(&recent) {
        if !tokens.contains(&token) {
            tokens.push(token);
        }
    }
    let mut scored = catalog
        .iter()
        .filter_map(|entry| {
            let project_id = value_text(entry.get("logicalProjectId"))?;
            let project_name = value_text(entry.get("projectName")).unwrap_or_default();
            let description = value_text(entry.get("description")).unwrap_or_default();
            let haystack = format!("{project_name} {description}").to_lowercase();
            let exact = (!project_name.is_empty()
                && query_lower.contains(&project_name.to_lowercase()))
                as usize
                * 2;
            let matched = tokens
                .iter()
                .filter(|token| haystack.contains(token.as_str()))
                .cloned()
                .collect::<Vec<_>>();
            let score = exact * 10 + matched.len();
            let updated_at = entry
                .get("summary")
                .and_then(|summary| summary.get("latestMemoryAt"))
                .and_then(JsValue::as_string)
                .map(|value| String::from_utf16_lossy(value.units()))
                .or_else(|| value_text(entry.get("updatedAt")))
                .unwrap_or_default();
            let source_type = match value_text(entry.get("sourceType")).as_deref() {
                Some("workspace_external") => "workspace_external",
                _ => "general_local",
            };
            Some((
                score,
                exact,
                updated_at.clone(),
                object(&[
                    ("projectId", string(project_id)),
                    ("projectName", string(project_name)),
                    ("description", string(description)),
                    (
                        "status",
                        entry.get("status").cloned().unwrap_or(JsValue::Undefined),
                    ),
                    ("updatedAt", string(updated_at)),
                    ("sourceType", JsValue::from(source_type)),
                    ("score", JsValue::Number(score as f64)),
                    ("exact", JsValue::Number(exact as f64)),
                    (
                        "source",
                        JsValue::from(if exact > 0 || !matched.is_empty() {
                            "query"
                        } else {
                            "recent"
                        }),
                    ),
                    ("matchedText", string(matched.join(", "))),
                ]),
            ))
        })
        .collect::<Vec<_>>();
    scored.sort_by(|left, right| right.0.cmp(&left.0).then_with(|| right.2.cmp(&left.2)));
    scored
        .into_iter()
        .take(30)
        .map(|(_, _, _, value)| value)
        .collect()
}

fn candidate_string_list(candidate: &JsObject, key: &str) -> Vec<String> {
    match candidate.get(key) {
        Some(JsValue::Array(values)) => values
            .iter()
            .filter_map(|value| value_text(Some(value)))
            .filter(|value| !value.trim().is_empty())
            .collect(),
        _ => Vec::new(),
    }
}

fn build_candidate_routing_query(candidate: &JsObject, focus_turn: &JsValue) -> String {
    let mut values = vec![
        value_text(focus_turn.get("content")).unwrap_or_default(),
        value_text(candidate.get("name")).unwrap_or_default(),
        value_text(candidate.get("description")).unwrap_or_default(),
        value_text(candidate.get("rule")).unwrap_or_default(),
        value_text(candidate.get("summary")).unwrap_or_default(),
        value_text(candidate.get("stage")).unwrap_or_default(),
    ];
    for key in ["constraints", "decisions", "blockers"] {
        values.extend(candidate_string_list(candidate, key));
    }
    values
        .into_iter()
        .filter(|value| !value.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

fn build_candidate_memory_preview(candidate: &JsObject) -> String {
    let name = value_text(candidate.get("name")).unwrap_or_default();
    let kind = value_text(candidate.get("type")).unwrap_or_default();
    let description = value_text(candidate.get("description")).unwrap_or_default();
    let mut lines = vec![
        format!("# {name}"),
        String::new(),
        format!("type: {kind}"),
        format!("description: {description}"),
    ];
    if kind == "feedback" {
        let rule = value_text(candidate.get("rule"))
            .or_else(|| value_text(candidate.get("description")))
            .or_else(|| value_text(candidate.get("summary")))
            .unwrap_or_else(|| name.clone());
        lines.extend([String::new(), "## Rule".into(), rule]);
        for (key, title) in [("why", "Why"), ("howToApply", "How To Apply")] {
            if let Some(value) = value_text(candidate.get(key)).filter(|value| !value.is_empty()) {
                lines.extend([String::new(), format!("## {title}"), value]);
            }
        }
    }
    if kind == "project" {
        if let Some(stage) = value_text(candidate.get("stage")).filter(|value| !value.is_empty()) {
            lines.extend([String::new(), "## Current Stage".into(), stage]);
        }
        for (key, title) in [
            ("decisions", "Decisions"),
            ("constraints", "Constraints"),
            ("nextSteps", "Next Steps"),
            ("blockers", "Blockers"),
            ("timeline", "Timeline"),
            ("notes", "Notes"),
        ] {
            let values = candidate_string_list(candidate, key);
            if !values.is_empty() {
                lines.extend([String::new(), format!("## {title}")]);
                lines.extend(
                    values
                        .into_iter()
                        .map(|value| format!("- {}", value.trim())),
                );
            }
        }
        if let Some(summary) =
            value_text(candidate.get("summary")).filter(|value| !value.is_empty())
        {
            lines.extend([String::new(), "## Summary".into(), summary]);
        }
    }
    if let Some(body) = value_text(candidate.get("body")).filter(|value| !value.is_empty()) {
        lines.extend([String::new(), "## Body".into(), body]);
    }
    String::from_utf16_lossy(truncate_utf16(lines.join("\n").trim(), 3000).units())
}

fn tokenize_search_text(value: &str) -> Vec<String> {
    const STOPWORDS: &[&str] = &[
        "项目", "当前", "这个", "那个", "现在", "一下", "关于", "里面", "这里", "那里", "怎么",
        "怎样", "如何", "什么", "哪些", "进展", "情况", "默认", "应该", "需要", "general",
    ];
    let normalized = value
        .to_lowercase()
        .chars()
        .map(|ch| if ch.is_alphanumeric() { ch } else { ' ' })
        .collect::<String>();
    let mut output = Vec::new();
    for token in normalized.split_whitespace() {
        let chars = token.chars().collect::<Vec<_>>();
        let has_han = chars
            .iter()
            .any(|ch| matches!(*ch as u32, 0x3400..=0x9fff | 0xf900..=0xfaff));
        let mut candidates = Vec::new();
        if !has_han || chars.len() <= 8 {
            candidates.push(token.to_owned());
        }
        if has_han {
            for size in [2, 3] {
                if chars.len() >= size {
                    for index in 0..=chars.len() - size {
                        candidates.push(chars[index..index + size].iter().collect());
                    }
                }
            }
        }
        for candidate in candidates {
            if candidate.chars().count() >= 2
                && !STOPWORDS.contains(&candidate.as_str())
                && !output.contains(&candidate)
            {
                output.push(candidate);
            }
        }
    }
    output
}

impl MemoryResolver for NativeEdgeClawRuntime {
    fn retrieve(&self, input: MemoryRetrieveInput) -> MemoryFuture {
        let runtime = self.clone();
        Box::pin(async move {
            let started_at = (runtime.inner.clock)();
            let result = match runtime.retrieve_now(&input).await {
                Ok(result) => result,
                Err(error) => {
                    return Ok(MemoryRetrieveResult {
                        system_context: None,
                        diagnostics: vec![MemoryDiagnostic {
                            code: "memory_provider_error".into(),
                            severity: "error".into(),
                            message: error,
                        }],
                        metadata: None,
                    });
                }
            };
            runtime
                .inner
                .pending_retrievals
                .lock()
                .expect("pending retrieval mutex poisoned")
                .insert(
                    input.session_id,
                    PendingRetrieval {
                        query: input.query,
                        started_at,
                        result: result.clone(),
                    },
                );
            Ok(result)
        })
    }

    fn capture_turn(&self, input: MemoryCaptureTurnInput) -> MemoryCaptureFuture {
        let runtime = self.clone();
        Box::pin(async move {
            // Match EdgeClawMemoryProvider: capture is observational and must
            // never reject the agent turn.
            let _ = runtime.capture_now(input);
            Ok(())
        })
    }
}

impl MemoryMaintenanceService for NativeEdgeClawRuntime {
    fn run_due_scheduled_maintenance(&self, reason: &str) -> MaintenanceFuture<'_, ()> {
        let runtime = self.clone();
        let reason = reason.to_owned();
        Box::pin(async move { runtime.run_maintenance(reason).await })
    }
}

impl MemoryManagementService for NativeEdgeClawRuntime {
    fn list(&self, limit: usize) -> Result<Vec<JsValue>, MemoryProviderError> {
        self.inner
            .stores
            .list_memory_entries(limit)
            .map_err(provider_error)
    }

    fn clear(&self, scope: &str) -> Result<(), MemoryProviderError> {
        if scope == "all_memory" {
            self.inner.stores.clear_all_memory_data()
        } else {
            self.inner.stores.clear_current_workspace_memory_data()
        }
        .map(|_| ())
        .map_err(provider_error)
    }

    fn clear_session(&self, session_key: &JsString) -> Result<(), MemoryProviderError> {
        let session = session_key
            .to_utf8()
            .map_err(|_| MemoryProviderError::host_required())?;
        self.inner
            .stores
            .clear_session_memory_data(&session)
            .map(|_| ())
            .map_err(provider_error)
    }
}

fn memory_messages(value: &JsValue) -> Vec<JsValue> {
    match value {
        JsValue::Array(messages) => messages.clone(),
        _ => Vec::new(),
    }
}

fn same_memory_message(left: &JsValue, right: &JsValue) -> bool {
    value_text(left.get("role")) == value_text(right.get("role"))
        && value_text(left.get("content")) == value_text(right.get("content"))
}

fn merge_session_messages(
    previous: &[JsValue],
    incoming: &[JsValue],
) -> (Vec<JsValue>, Vec<JsValue>) {
    if previous.is_empty() {
        return (incoming.to_vec(), incoming.to_vec());
    }
    let common_prefix = previous
        .iter()
        .zip(incoming)
        .take_while(|(left, right)| same_memory_message(left, right))
        .count();
    if common_prefix > 0 {
        return (incoming.to_vec(), incoming[common_prefix..].to_vec());
    }
    let mut merged = previous.to_vec();
    merged.extend_from_slice(incoming);
    (merged, incoming.to_vec())
}

fn canonical_messages(messages: &[JsValue], include_fork_carryover: bool) -> Vec<JsValue> {
    let mut result = Vec::new();
    for (message_index, message) in messages.iter().enumerate() {
        if !include_fork_carryover
            && message
                .get("metadata")
                .and_then(|metadata| metadata.get("forkCarryover"))
                .is_some_and(js_truthy)
        {
            continue;
        }
        let role = value_text(message.get("role")).unwrap_or_default();
        let mut entries: Vec<(String, String)> = Vec::new();
        if let Some(JsValue::Array(blocks)) = message.get("content") {
            for block in blocks {
                let kind = value_text(block.get("type")).unwrap_or_default();
                let (entry_role, content) = match kind.as_str() {
                    "text" => (
                        role.as_str(),
                        value_text(block.get("text")).unwrap_or_default(),
                    ),
                    "tool_result" => {
                        let content = match block.get("content") {
                            Some(JsValue::Array(items)) => items
                                .iter()
                                .map(|item| {
                                    if value_text(item.get("type")).as_deref() == Some("text") {
                                        value_text(item.get("text")).unwrap_or_default()
                                    } else {
                                        format!(
                                            "[{}]",
                                            value_text(item.get("type")).unwrap_or_default()
                                        )
                                    }
                                })
                                .collect::<Vec<_>>()
                                .join("\n"),
                            _ => String::new(),
                        };
                        ("tool", content)
                    }
                    "tool_result_reference" | "media_reference" => {
                        ("tool", value_text(block.get("preview")).unwrap_or_default())
                    }
                    _ => continue,
                };
                let content = js_trim(&content).to_owned();
                if content.is_empty() {
                    continue;
                }
                if let Some((previous_role, previous)) = entries.last_mut()
                    && previous_role == entry_role
                {
                    previous.push('\n');
                    previous.push_str(&content);
                } else {
                    entries.push((entry_role.to_owned(), content));
                }
            }
        }
        let count = entries.len();
        for (entry_index, (role, content)) in entries.into_iter().enumerate() {
            result.push(object(&[
                (
                    "msgId",
                    string(if count == 1 {
                        format!("message-{message_index}")
                    } else {
                        format!("message-{message_index}:{entry_index}")
                    }),
                ),
                ("role", string(role)),
                ("content", string(content)),
            ]));
        }
    }
    result
}

fn normalize_capture_messages(
    messages: &[JsValue],
    capture_strategy: CaptureStrategy,
    include_assistant: bool,
    max_message_chars: usize,
) -> Vec<JsValue> {
    let mut normalized = Vec::new();
    for message in messages {
        let role = value_text(message.get("role")).unwrap_or_default();
        if role != "user" && role != "assistant" {
            continue;
        }
        if role == "assistant" && !include_assistant {
            continue;
        }
        let raw = value_text(message.get("content")).unwrap_or_default();
        let content = if role == "assistant" {
            normalize_assistant_capture(&raw)
        } else {
            normalize_user_capture(&raw)
        };
        if content.is_empty()
            || (role == "user" && should_skip_user_capture(&content))
            || (role == "assistant" && should_skip_assistant_capture(&content))
        {
            continue;
        }
        let mut fields = vec![
            ("role", string(role)),
            (
                "content",
                JsValue::String(truncate_utf16(&content, max_message_chars)),
            ),
        ];
        if let Some(id) = message.get("msgId").and_then(JsValue::as_string) {
            fields.push(("msgId", JsValue::String(id.clone())));
        }
        normalized.push(object(&fields));
    }

    if capture_strategy == CaptureStrategy::FullSession {
        return normalized;
    }
    if let Some(last_user) = normalized
        .iter()
        .rposition(|message| value_text(message.get("role")).as_deref() == Some("user"))
    {
        normalized.split_off(last_user)
    } else {
        let keep = normalized.len().min(2);
        normalized.split_off(normalized.len() - keep)
    }
}

fn normalize_user_capture(value: &str) -> String {
    compact_capture_whitespace(&strip_user_noise(value))
}

fn strip_user_noise(value: &str) -> String {
    let mut value = strip_injected_memory_context(value);
    if let Some((_, current)) = value.rsplit_once("[Current message - respond to this]") {
        value = current.trim().to_owned();
    }
    value = strip_quoted_context_blocks(&value);
    value = Regex::new(r"(?im)^\[media attached:[^\n]*\]\s*")
        .expect("valid media regex")
        .replace_all(&value, "")
        .into_owned();
    value = Regex::new(r"(?im)^To send an image back,[^\n]*$")
        .expect("valid image instruction regex")
        .replace_all(&value, "")
        .into_owned();
    if let Some(index) = value.to_lowercase().find("\n<file") {
        value.truncate(index);
    }
    value = Regex::new(r"(?i)<file\b[^>]*>")
        .expect("valid file tag regex")
        .replace_all(&value, "")
        .into_owned();
    value = strip_untrusted_metadata(&value);
    value = strip_leading_current_message_envelope(&value);
    value = strip_leading_timestamp(&value);
    value = strip_leading_envelope_lines(&value);
    value = strip_leading_sender(&value);
    value = Regex::new(r"(?i)^(?:@[\w.-]+\s*)+")
        .expect("valid mention regex")
        .replace(&value, "")
        .into_owned();
    value = strip_plugin_state_scaffolding(&value);
    value = strip_leading_envelope_lines(&value);
    value = strip_leading_timestamp(&value);
    compact_capture_whitespace(&value)
}

fn strip_injected_memory_context(value: &str) -> String {
    const HEADERS: &[&str] = &[
        "You are using retrieved ClawXMemory file memories for this turn.",
        "You are using multi-level memory indexes for this turn.",
    ];
    const FOOTER: &str = "Treat the above as authoritative prior memory when it is relevant. Prioritize the user's latest request, and do not claim memory is missing or that this is a fresh conversation if the answer is already shown above.";
    let mut cleaned = value.trim().to_owned();
    for header in HEADERS {
        while let Some(start) = cleaned.find(header) {
            let Some(end) = cleaned[start + header.len()..].find(FOOTER) else {
                break;
            };
            let end = start + header.len() + end + FOOTER.len();
            cleaned.replace_range(start..end, "");
        }
    }
    const RECALL_HEADER: &str = "## ClawXMemory Recall";
    const RECALL_INSTRUCTION: &str =
        "Use the following retrieved ClawXMemory evidence for this turn.";
    const RECALL_FOOTER: &str = "Treat the selected evidence above as authoritative historical memory for this turn when it is relevant.";
    const RECALL_FINAL: &str = "If the needed answer is already shown above, do not claim that memory is missing or that this is a fresh conversation.";
    while let Some(start) = cleaned.find(RECALL_HEADER) {
        if !cleaned[start..].contains(RECALL_INSTRUCTION) {
            break;
        }
        let Some(end) = cleaned[start..].find(RECALL_FOOTER) else {
            break;
        };
        let end = start + end + RECALL_FOOTER.len();
        let final_end = cleaned[end..]
            .find(RECALL_FINAL)
            .map(|offset| end + offset + RECALL_FINAL.len())
            .unwrap_or(end);
        cleaned.replace_range(start..final_end, "");
    }
    cleaned.trim().to_owned()
}

fn strip_quoted_context_blocks(value: &str) -> String {
    let mut cleaned = Regex::new(
        r"(?is)(?:Chat history since last reply|Replied message)\s*\(untrusted[^)]*\)\s*:\s*```(?:json)?[\s\S]*?```\s*",
    )
    .expect("valid quoted context regex")
    .replace_all(value, "")
    .into_owned();
    const HISTORY: &str = "[Chat messages since your last reply - for context]";
    const CURRENT: &str = "[Current message - respond to this]";
    while let Some(start) = cleaned.find(HISTORY) {
        let end = cleaned[start + HISTORY.len()..]
            .find(CURRENT)
            .map(|offset| start + HISTORY.len() + offset)
            .unwrap_or(cleaned.len());
        cleaned.replace_range(start..end, "");
    }
    cleaned
}

fn strip_untrusted_metadata(value: &str) -> String {
    let fenced = Regex::new(
        r"(?is)(?:Conversation info|Sender)\s*\(untrusted metadata\)\s*:\s*```(?:json)?[\s\S]*?```\s*",
    )
    .expect("valid metadata fence regex")
    .replace_all(value, "")
    .into_owned();
    Regex::new(r"(?is)(?:Conversation info|Sender)\s*\(untrusted metadata\)\s*:\s*\{[^{}]*\}\s*")
        .expect("valid inline metadata regex")
        .replace_all(&fenced, "")
        .into_owned()
}

fn strip_leading_current_message_envelope(value: &str) -> String {
    let regex = Regex::new(r"(?s)^(?:\[[^\]\n]{1,320}\]\s*)+([^:\n]{1,120}):\s*([\s\S]*)$")
        .expect("valid current message envelope regex");
    regex
        .captures(value)
        .and_then(|captures| captures.get(2))
        .map(|value| value.as_str().trim().to_owned())
        .unwrap_or_else(|| value.to_owned())
}

fn strip_leading_timestamp(value: &str) -> String {
    Regex::new(r"(?i)^\s*\[[^\]\n]*(?:\d{4}-\d{1,2}-\d{1,2}|GMT|UTC)[^\]\n]*\]\s*")
        .expect("valid timestamp regex")
        .replace(value, "")
        .into_owned()
}

fn strip_leading_envelope_lines(value: &str) -> String {
    let mut lines = value.lines().collect::<Vec<_>>();
    while lines.first().is_some_and(|line| envelope_noise_line(line)) {
        lines.remove(0);
    }
    lines.join("\n").trim().to_owned()
}

fn envelope_noise_line(line: &str) -> bool {
    let value = line.trim();
    value.is_empty()
        || value == "```"
        || value.eq_ignore_ascii_case("```json")
        || value == "{"
        || value == "}"
        || Regex::new(r"(?i)^(?:Conversation info|Sender)\s*\(untrusted metadata\)\s*:?")
            .expect("valid metadata line regex")
            .is_match(value)
        || Regex::new(
            r"(?i)^(?:Chat history since last reply|Replied message)\s*\(untrusted[^)]*\)\s*:?",
        )
        .expect("valid quoted line regex")
        .is_match(value)
        || Regex::new(r"(?i)^System:\s*\[[^\]\n]*(?:\d{4}-\d{1,2}-\d{1,2}|GMT|UTC)[^\]\n]*\]")
            .expect("valid system timestamp regex")
            .is_match(value)
        || Regex::new(r"(?i)^\[message_id:[^\]]+\]$")
            .expect("valid message id regex")
            .is_match(value)
        || (value.starts_with('[')
            && (value.contains("for context")
                || value.contains("Current message - respond to this")))
}

fn strip_leading_sender(value: &str) -> String {
    let mut lines = value.lines();
    let Some(first) = lines.next() else {
        return String::new();
    };
    let regex = Regex::new(r"^([^:\n]{1,120}):\s*(.*)$").expect("valid sender regex");
    let Some(captures) = regex.captures(first) else {
        return value.to_owned();
    };
    let label = captures
        .get(1)
        .map(|value| value.as_str())
        .unwrap_or_default();
    let opaque = label.is_ascii()
        && label
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || "_.@()- ".contains(ch))
        && (label.chars().any(|ch| ch.is_ascii_digit() || ch == '_') || label.len() >= 5);
    if !opaque {
        return value.to_owned();
    }
    std::iter::once(
        captures
            .get(2)
            .map(|value| value.as_str())
            .unwrap_or_default(),
    )
    .chain(lines)
    .collect::<Vec<_>>()
    .join("\n")
    .trim()
    .to_owned()
}

fn strip_plugin_state_scaffolding(value: &str) -> String {
    let normalized = compact_capture_whitespace(value);
    let mut paragraphs = normalized
        .split("\n\n")
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>();
    while paragraphs
        .first()
        .is_some_and(|paragraph| plugin_noise_paragraph(paragraph))
    {
        paragraphs.remove(0);
    }
    let joined = paragraphs.join("\n\n");
    let mut lines = joined.lines().collect::<Vec<_>>();
    while lines.first().is_some_and(|line| plugin_noise_line(line)) {
        lines.remove(0);
    }
    let cleaned = compact_capture_whitespace(&lines.join("\n"));
    if plugin_noise_paragraph(&cleaned) {
        String::new()
    } else {
        cleaned
    }
}

fn normalize_plugin_probe(value: &str) -> String {
    value
        .replace('\r', "")
        .replace("**", "")
        .replace('`', "")
        .lines()
        .map(|line| line.trim_start_matches(|ch: char| ">-* ".contains(ch)))
        .collect::<Vec<_>>()
        .join(" ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn plugin_marker_count(value: &str) -> usize {
    let value = normalize_plugin_probe(value).to_lowercase();
    [
        "project state maintained by",
        "session status",
        "agent:",
        "host:",
        "workspace:",
        "os:",
        "node:",
        "model:",
        "current git branch:",
        "git status:",
        "git status summary:",
    ]
    .iter()
    .filter(|marker| value.contains(**marker))
    .count()
}

fn plugin_noise_line(value: &str) -> bool {
    let value = normalize_plugin_probe(value).to_lowercase();
    value.starts_with("project state maintained by")
        || value.starts_with("session status")
        || value.starts_with("current git branch:")
        || value.starts_with("git status:")
        || value.starts_with("git status summary:")
        || plugin_marker_count(&value) >= 4
}

fn plugin_noise_paragraph(value: &str) -> bool {
    if value.is_empty() || plugin_noise_line(value) {
        return !value.is_empty();
    }
    let lines = value
        .lines()
        .filter(|line| !line.trim().is_empty())
        .collect::<Vec<_>>();
    let noisy = lines
        .iter()
        .filter(|line| plugin_noise_line(line) || plugin_marker_count(line) >= 3)
        .count();
    let markers = plugin_marker_count(value);
    (value.to_lowercase().contains("session status") && markers >= 2)
        || (markers >= 4 && noisy >= lines.len().div_ceil(2).max(1))
}

fn normalize_assistant_capture(value: &str) -> String {
    let mut value = value.to_owned();
    for pattern in [r"(?is)^.*?</think>", r"(?is)^.*?</thinking>"] {
        value = Regex::new(pattern)
            .expect("valid assistant regex")
            .replace(&value, "")
            .into();
    }
    for pattern in [
        r"(?is)<think>.*?</think>",
        r"(?is)<thinking>.*?</thinking>",
        r"(?i)</?think[^>]*>",
        r"(?i)</?thinking[^>]*>",
    ] {
        value = Regex::new(pattern)
            .expect("valid assistant regex")
            .replace_all(&value, "")
            .into();
    }
    let normalized = compact_capture_whitespace(&value);
    let paragraphs = normalized
        .split("\n\n")
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>();
    if paragraphs.len() >= 2 && assistant_meta_reasoning(paragraphs[0]) {
        strip_plugin_state_scaffolding(&paragraphs[1..].join("\n\n"))
    } else {
        strip_plugin_state_scaffolding(&normalized)
    }
}

fn compact_capture_whitespace(value: &str) -> String {
    let mut result = Vec::new();
    let mut previous_empty = false;
    let without_cr = value.replace('\r', "");
    for line in without_cr.lines() {
        let line = line.trim_end();
        let empty = line.trim().is_empty();
        if empty && previous_empty {
            continue;
        }
        result.push(if empty { "" } else { line });
        previous_empty = empty;
    }
    result.join("\n").trim().to_owned()
}

fn assistant_meta_reasoning(value: &str) -> bool {
    (value.starts_with('用')
        && value.starts_with("用户")
        && ["需要", "应该", "这是", "表达", "分享", "说明", "记录"]
            .iter()
            .any(|needle| value.contains(needle)))
        || (value.starts_with("搜索结果")
            && ["需要", "无法", "不太理想"]
                .iter()
                .any(|needle| value.contains(needle)))
        || (value.starts_with("这搜索")
            && ["不太全", "看不到", "找不到"]
                .iter()
                .any(|needle| value.contains(needle)))
}

fn should_skip_assistant_capture(value: &str) -> bool {
    if value.contains('\n') {
        return false;
    }
    assistant_meta_reasoning(value)
}

fn should_skip_user_capture(value: &str) -> bool {
    const SLUG_PROMPT: &str = "Based on this conversation, generate a short 1-2 word filename slug";
    const SESSION_START: &str = "A new session was started via /new or /reset.";
    if value.contains(SESSION_START)
        || value.starts_with(SLUG_PROMPT)
        || (Regex::new(r"(?i)\b(?:Run|Execute) your Session Startup sequence\b")
            .expect("valid startup sequence regex")
            .is_match(value)
            && value.contains(SESSION_START))
    {
        return true;
    }
    let command = value
        .strip_prefix('/')
        .and_then(|rest| {
            rest.split(|ch: char| ch.is_whitespace() || ch == ':')
                .next()
        })
        .unwrap_or_default()
        .to_ascii_lowercase();
    const COMMANDS: &[&str] = &[
        "help",
        "commands",
        "skill",
        "status",
        "allowlist",
        "approve",
        "context",
        "whoami",
        "id",
        "subagents",
        "config",
        "debug",
        "usage",
        "tts",
        "voice",
        "stop",
        "restart",
        "dock-telegram",
        "dock_telegram",
        "dock-discord",
        "dock_discord",
        "dock-slack",
        "dock_slack",
        "activation",
        "send",
        "reset",
        "new",
        "think",
        "thinking",
        "t",
        "verbose",
        "v",
        "reasoning",
        "reason",
        "elevated",
        "elev",
        "exec",
        "model",
        "models",
        "queue",
        "bash",
        "compact",
    ];
    if value.starts_with('/') && COMMANDS.contains(&command.as_str()) {
        return true;
    }
    value.starts_with("!poll") || value.starts_with("!stop") || value.starts_with("! ")
}

fn truncate_utf16(value: &str, max_chars: usize) -> JsString {
    let units = value.encode_utf16().collect::<Vec<_>>();
    if max_chars == 0 || units.len() <= max_chars {
        return JsString::from_utf16(units);
    }
    let mut truncated = units.into_iter().take(max_chars).collect::<Vec<_>>();
    truncated.extend("...".encode_utf16());
    JsString::from_utf16(truncated)
}

fn has_new_capture_content(previous: &JsValue, incoming: &[JsValue]) -> bool {
    let previous = match previous {
        JsValue::Array(previous) => previous,
        _ => return !incoming.is_empty(),
    };
    if incoming.is_empty() {
        return false;
    }
    if previous.is_empty() || incoming.len() > previous.len() {
        return true;
    }
    incoming.iter().enumerate().any(|(index, message)| {
        let Some(prior) = previous.get(index) else {
            return true;
        };
        prior.get("role") != message.get("role") || prior.get("content") != message.get("content")
    })
}

fn render_system_context(
    route: &str,
    user_summary: Option<&JsValue>,
    project_meta: Option<&JsValue>,
    records: &[JsValue],
) -> String {
    let identity = user_summary
        .and_then(|summary| summary.get("identityBackground"))
        .and_then(|value| match value {
            JsValue::Array(values) => Some(
                values
                    .iter()
                    .filter_map(JsValue::as_string)
                    .map(|value| String::from_utf16_lossy(value.units()))
                    .filter(|value| !js_trim(value).is_empty())
                    .collect::<Vec<_>>(),
            ),
            _ => None,
        })
        .unwrap_or_default();
    let has_user = !identity.is_empty();

    let user_summary_paths: std::collections::HashSet<String> = user_summary
        .and_then(|summary| summary.get("files"))
        .and_then(|files| match files {
            JsValue::Array(arr) => Some(
                arr.iter()
                    .filter_map(|f| value_text(f.get("relativePath")))
                    .collect(),
            ),
            _ => None,
        })
        .unwrap_or_default();

    let unique_records: Vec<&JsValue> = records
        .iter()
        .filter(|record| {
            if let Some(path) = value_text(record.get("relativePath")) {
                !user_summary_paths.contains(&path)
            } else {
                true
            }
        })
        .collect();

    // Check preconditions matching TS renderContext lines 173-187
    if route == "user" {
        if !has_user {
            return String::new();
        }
    } else if route == "project" {
        if project_meta.is_none() && unique_records.is_empty() {
            return String::new();
        }
    } else if route == "mix" {
        if !has_user && project_meta.is_none() && unique_records.is_empty() {
            return String::new();
        }
    } else {
        return String::new();
    }

    let mut lines = vec![
        "## ClawXMemory Recall".to_string(),
        format!("route={route}"),
        "".to_string(),
    ];

    let render_user_summary_block = |lines: &mut Vec<String>| {
        if !has_user {
            return;
        }
        let first_file = user_summary
            .and_then(|s| s.get("files"))
            .and_then(|f| match f {
                JsValue::Array(arr) => arr.first(),
                _ => None,
            });
        let updated_at = first_file
            .and_then(|f| value_text(f.get("updatedAt")))
            .unwrap_or_default();
        let relative_path = first_file
            .and_then(|f| value_text(f.get("relativePath")))
            .unwrap_or_else(|| "global/UserIdentity/user-profile.md".into());
        let header = if !updated_at.is_empty() {
            format!("### [user] {relative_path} ({updated_at})")
        } else {
            format!("### [user] {relative_path}")
        };
        lines.push(header);
        lines.push("## 身份背景".to_string());
        for item in &identity {
            lines.push(format!("- {item}"));
        }
        lines.push("".to_string());
    };

    let render_project_meta_block = |lines: &mut Vec<String>| {
        if let Some(meta) = project_meta {
            let rel_path = value_text(meta.get("relativePath"))
                .unwrap_or_else(|| "projects/project.md".into());
            let updated_at = value_text(meta.get("updatedAt")).unwrap_or_default();
            let name = value_text(meta.get("projectName")).unwrap_or_default();
            let description = value_text(meta.get("description"))
                .filter(|v| !v.is_empty())
                .unwrap_or_else(|| "No project description yet.".into());
            let status = value_text(meta.get("status")).unwrap_or_default();

            lines.push(format!("### [project_meta] {rel_path} ({updated_at})"));
            lines.push("## Project Name".to_string());
            lines.push(name);
            lines.push("".to_string());
            lines.push("## Description".to_string());
            lines.push(description);
            lines.push("".to_string());
            lines.push("## Status".to_string());
            lines.push(status);
            lines.push("".to_string());
        }
    };

    let render_selected_records_block = |lines: &mut Vec<String>| {
        for record in &unique_records {
            let ty = value_text(record.get("type")).unwrap_or_else(|| "project".into());
            let rel_path = value_text(record.get("relativePath")).unwrap_or_default();
            let updated_at = value_text(record.get("updatedAt")).unwrap_or_default();
            let content = value_text(record.get("content")).unwrap_or_default();
            lines.push(format!("### [{ty}] {rel_path} ({updated_at})"));
            lines.push(content.trim().to_string());
            lines.push("".to_string());
        }
    };

    if route == "user" {
        render_user_summary_block(&mut lines);
    } else if route == "project" {
        render_project_meta_block(&mut lines);
        render_selected_records_block(&mut lines);
    } else if route == "mix" {
        render_user_summary_block(&mut lines);
        render_project_meta_block(&mut lines);
        render_selected_records_block(&mut lines);
    }

    lines.push("Treat these file memories as the authoritative long-term memory for this turn when relevant.".to_string());
    lines.join("\n").trim().to_string()
}

fn utf16_len(value: &str) -> usize {
    value.encode_utf16().count()
}

fn last_assistant_text(messages: &[JsValue]) -> String {
    messages
        .iter()
        .rev()
        .find(|message| value_text(message.get("role")).as_deref() == Some("assistant"))
        .and_then(|message| value_text(message.get("content")))
        .unwrap_or_default()
}

fn value_text(value: Option<&JsValue>) -> Option<String> {
    value
        .and_then(JsValue::as_string)
        .map(|value| String::from_utf16_lossy(value.units()))
}

fn non_negative_integer(value: Option<&JsValue>) -> Option<i64> {
    value
        .and_then(JsValue::as_number)
        .filter(|value| value.is_finite() && *value >= 0.0)
        .map(|value| value.floor().min(i64::MAX as f64) as i64)
}

fn js_truthy(value: &JsValue) -> bool {
    match value {
        JsValue::Undefined | JsValue::Null => false,
        JsValue::Bool(value) => *value,
        JsValue::Number(value) => *value != 0.0 && !value.is_nan(),
        JsValue::String(value) => !value.units().is_empty(),
        JsValue::Array(_) | JsValue::Object(_) => true,
    }
}

fn elapsed_minutes(anchor: &str, now: &str) -> i64 {
    let parse = |value: &str| {
        pilotdeck_transport::system_clock::parse_node22_date(&JsString::from(value))
            .ok()
            .flatten()
    };
    let Some(anchor) = parse(anchor) else {
        return i64::MIN;
    };
    let Some(now) = parse(now) else {
        return i64::MIN;
    };
    ((now - anchor) / 60_000.0)
        .floor()
        .clamp(i64::MIN as f64, i64::MAX as f64) as i64
}

fn dream_trace(
    started_at: &str,
    trigger: &str,
    status: &str,
    summary: &str,
    changed: Vec<JsValue>,
) -> JsValue {
    let changed_count = changed.len();
    object(&[
        (
            "dreamTraceId",
            string(format!("dream_trace_{}", uuid::Uuid::new_v4().simple())),
        ),
        ("trigger", JsValue::from(trigger)),
        ("startedAt", JsValue::from(started_at)),
        ("finishedAt", string(now_iso())),
        ("status", JsValue::from(status)),
        ("isNoOp", JsValue::Bool(changed_count == 0)),
        (
            "displayStatus",
            JsValue::from(if status == "error" {
                "Error"
            } else if changed_count == 0 {
                "No-op"
            } else {
                "Completed"
            }),
        ),
        (
            "snapshotSummary",
            object(&[
                ("projectMetaPresent", JsValue::Bool(false)),
                ("projectFileCount", JsValue::Number(0.0)),
                ("feedbackFileCount", JsValue::Number(0.0)),
                ("hasUserProfile", JsValue::Bool(false)),
            ]),
        ),
        ("steps", JsValue::Array(Vec::new())),
        ("mutations", JsValue::Array(changed)),
        (
            "outcome",
            object(&[
                ("rewrittenProjects", JsValue::Number(changed_count as f64)),
                ("deletedProjects", JsValue::Number(0.0)),
                ("deletedFiles", JsValue::Number(0.0)),
                ("profileUpdated", JsValue::Bool(false)),
                ("summary", string(summary)),
            ]),
        ),
    ])
}

fn object(fields: &[(&str, JsValue)]) -> JsValue {
    let mut object = JsObject::new();
    for (key, value) in fields {
        object.insert((*key).into(), value.clone());
    }
    JsValue::Object(object)
}

fn string(value: impl Into<JsString>) -> JsValue {
    JsValue::String(value.into())
}

fn normalize_whitespace(value: &str) -> String {
    let mut words = value.split_whitespace();
    let mut result = String::new();
    if let Some(first) = words.next() {
        result.push_str(first);
        for w in words {
            result.push(' ');
            result.push_str(w);
        }
    }
    result
}

fn truncate_for_prompt(s: &str, max_chars: usize) -> String {
    let normalized = normalize_whitespace(s);
    let chars: Vec<char> = normalized.chars().collect();
    if chars.len() <= max_chars {
        normalized
    } else {
        chars[..max_chars]
            .iter()
            .collect::<String>()
            .trim()
            .to_string()
    }
}

fn trace_i18n(key: &str, fallback: &str) -> JsValue {
    object(&[
        ("key", JsValue::from(key)),
        ("fallback", JsValue::from(fallback)),
    ])
}

fn kv_detail(
    key: &str,
    label: &str,
    entries: Vec<(&str, JsValue)>,
    label_i18n: Option<JsValue>,
) -> JsValue {
    let mut fields = vec![
        ("key", JsValue::from(key)),
        ("label", JsValue::from(label)),
        ("kind", JsValue::from("kv")),
        (
            "entries",
            JsValue::Array(
                entries
                    .into_iter()
                    .map(|(k, v)| object(&[("label", JsValue::from(k)), ("value", v)]))
                    .collect(),
            ),
        ),
    ];
    if let Some(i18n) = label_i18n {
        fields.push(("labelI18n", i18n));
    }
    object(&fields)
}

fn list_detail(key: &str, label: &str, items: Vec<String>, label_i18n: Option<JsValue>) -> JsValue {
    let mut fields = vec![
        ("key", JsValue::from(key)),
        ("label", JsValue::from(label)),
        ("kind", JsValue::from("list")),
        (
            "items",
            JsValue::Array(items.into_iter().map(string).collect()),
        ),
    ];
    if let Some(i18n) = label_i18n {
        fields.push(("labelI18n", i18n));
    }
    object(&fields)
}

fn now_iso() -> String {
    pilotdeck_transport::system_clock::format_epoch_iso(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis()
            .min(i64::MAX as u128) as i64,
    )
}

fn provider_error(error: FileMemoryError) -> MemoryProviderError {
    MemoryProviderError::new(error.to_string())
}

fn storage_error(error: super::StorageError) -> MemoryProviderError {
    MemoryProviderError::new(error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{host::Cancellation, runtime::context::memory::MemoryResolver};
    use std::collections::VecDeque;

    struct ScriptedLlm {
        responses: Mutex<VecDeque<(&'static str, Result<JsValue, String>)>>,
        calls: Mutex<Vec<(&'static str, JsValue)>>,
    }

    impl ScriptedLlm {
        fn new(responses: Vec<(&'static str, Result<JsValue, String>)>) -> Arc<Self> {
            Arc::new(Self {
                responses: Mutex::new(responses.into()),
                calls: Mutex::new(Vec::new()),
            })
        }
    }

    impl NativeEdgeClawLlm for ScriptedLlm {
        fn execute(&self, operation: &'static str, input: JsValue) -> NativeEdgeClawLlmFuture {
            self.calls.lock().unwrap().push((operation, input));
            let (expected, response) = self
                .responses
                .lock()
                .unwrap()
                .pop_front()
                .expect("scripted memory LLM response");
            assert_eq!(operation, expected);
            Box::pin(async move { response })
        }
    }

    fn runtime() -> (crate::scratch::Scratch, NativeEdgeClawRuntime) {
        runtime_with(|options| options)
    }

    fn runtime_with(
        configure: impl FnOnce(NativeEdgeClawOptions) -> NativeEdgeClawOptions,
    ) -> (crate::scratch::Scratch, NativeEdgeClawRuntime) {
        let scratch = crate::scratch::Scratch::new("native-edgeclaw-provider");
        let project = scratch.path().join("project");
        std::fs::create_dir(&project).unwrap();
        let options = configure(NativeEdgeClawOptions::new(
            scratch.path().join("memory-root"),
            scratch.path().join("general"),
        ));
        let runtime = NativeEdgeClawRuntime::open(&project, options).unwrap();
        (scratch, runtime)
    }

    fn message(role: &str, text: &str, carryover: bool) -> JsValue {
        object(&[
            ("role", JsValue::from(role)),
            (
                "content",
                JsValue::Array(vec![object(&[
                    ("type", JsValue::from("text")),
                    ("text", JsValue::from(text)),
                ])]),
            ),
            (
                "metadata",
                object(&[("forkCarryover", JsValue::Bool(carryover))]),
            ),
        ])
    }

    #[test]
    fn canonical_messages_excludes_truthy_object_fork_carryover() {
        let inherited = object(&[
            ("role", JsValue::from("user")),
            (
                "content",
                JsValue::Array(vec![object(&[
                    ("type", JsValue::from("text")),
                    ("text", JsValue::from("inherited")),
                ])]),
            ),
            (
                "metadata",
                object(&[(
                    "forkCarryover",
                    object(&[("source", JsValue::from("parent"))]),
                )]),
            ),
        ]);
        let messages = [inherited, message("user", "local", false)];
        let normalized = canonical_messages(&messages, false);
        assert_eq!(normalized.len(), 1);
        assert_eq!(
            value_text(normalized[0].get("content")).as_deref(),
            Some("local")
        );
        let retrieval = canonical_messages(&messages, true);
        assert_eq!(retrieval.len(), 2);
        assert_eq!(
            value_text(retrieval[0].get("content")).as_deref(),
            Some("inherited")
        );
    }

    #[test]
    fn dream_user_note_window_enforces_file_and_utf16_budgets() {
        let records = (0..201)
            .map(|index| {
                object(&[
                    (
                        "relativePath",
                        string(format!("global/UserIdentityNotes/{index:03}.md")),
                    ),
                    ("updatedAt", string(format!("2026-10-31T00:{index:03}:00Z"))),
                    ("content", JsValue::from("x")),
                ])
            })
            .collect::<Vec<_>>();
        let selected = select_dream_user_note_window(&records);
        assert_eq!(selected.len(), 200);
        assert!(!selected.iter().any(|record| {
            value_text(record.get("relativePath")).as_deref()
                == Some("global/UserIdentityNotes/000.md")
        }));

        let oversized = vec![
            object(&[
                (
                    "relativePath",
                    JsValue::from("global/UserIdentityNotes/new.md"),
                ),
                ("updatedAt", JsValue::from("2026-10-31T02:00:00Z")),
                ("content", string("x".repeat(120_000))),
            ]),
            object(&[
                (
                    "relativePath",
                    JsValue::from("global/UserIdentityNotes/old.md"),
                ),
                ("updatedAt", JsValue::from("2026-10-31T01:00:00Z")),
                ("content", JsValue::from("y")),
            ]),
        ];
        assert_eq!(select_dream_user_note_window(&oversized).len(), 1);
    }

    #[test]
    fn dream_cluster_validation_rejects_overlap_size_generic_and_cross_project() {
        let records = (0..12)
            .map(|index| {
                object(&[
                    ("relativePath", string(format!("Project/{index}.md"))),
                    (
                        "projectId",
                        JsValue::from(if index < 10 { "alpha" } else { "beta" }),
                    ),
                    ("updatedAt", string(format!("2026-10-31T00:{index:02}:00Z"))),
                ])
            })
            .collect::<Vec<_>>();
        let members = |ids: &[usize], reason: &str| {
            object(&[
                (
                    "member_relative_paths",
                    JsValue::Array(
                        ids.iter()
                            .map(|id| string(format!("Project/{id}.md")))
                            .collect(),
                    ),
                ),
                ("reason", JsValue::from(reason)),
            ])
        };
        let plan = object(&[(
            "project_clusters",
            JsValue::Array(vec![
                members(&[0, 1], "Duplicate delivery facts"),
                members(&[1, 2], "Overlapping delivery facts"),
                members(&[0, 1, 2, 3, 4, 5, 6, 7, 8], "Duplicate facts"),
                members(&[10, 11], "They belong to the same project"),
                members(&[2, 10], "Duplicate facts"),
            ]),
        )]);
        let (accepted, warnings) = validate_dream_clusters(&plan, "project", &records);
        assert_eq!(accepted.len(), 1);
        assert_eq!(accepted[0].member_ids.len(), 2);
        assert_eq!(warnings.len(), 4);
    }

    #[tokio::test]
    async fn capture_strips_transport_envelopes_and_plugin_state_scaffolding() {
        let (_scratch, runtime) = runtime_with(|options| {
            options.with_capture_policy(CaptureStrategy::FullSession, true, 6_000)
        });
        runtime
            .capture_turn(MemoryCaptureTurnInput {
                session_id: "noise".into(),
                project_root: "/project".into(),
                messages: vec![
                    message(
                        "user",
                        "Conversation info (untrusted metadata):\n```json\n{\"sender\":\"user_42\"}\n```\n[Current message - respond to this]\n[2026-10-31 09:00 UTC] user_42: @pilot remember the release checklist",
                        false,
                    ),
                    message(
                        "assistant",
                        "Project state maintained by plugin\nAgent: alpha\nHost: local\nWorkspace: /tmp\nOS: macOS\n\nSaved the release checklist.",
                        false,
                    ),
                ],
                errored: false,
            })
            .await
            .unwrap();
        let rows = runtime.inner.stores.storage.list_all_l0().unwrap();
        let JsValue::Array(messages) = &rows[0].messages else {
            panic!("capture messages must be an array")
        };
        assert_eq!(messages.len(), 2);
        assert_eq!(
            value_text(messages[0].get("content")).as_deref(),
            Some("remember the release checklist")
        );
        assert_eq!(
            value_text(messages[1].get("content")).as_deref(),
            Some("Saved the release checklist.")
        );
    }

    #[tokio::test]
    async fn recall_cache_reuses_same_snapshot_and_marks_debug_metadata() {
        let llm = ScriptedLlm::new(vec![
            (
                "decide_file_memory_route",
                Ok(object(&[("route", JsValue::from("user"))])),
            ),
            (
                "select_file_manifest_entries",
                Ok(object(&[("selected_ids", JsValue::Array(Vec::new()))])),
            ),
        ]);
        let (_scratch, runtime) = runtime_with(|options| options.with_llm(llm.clone()));
        runtime
            .inner
            .stores
            .global_user_memory
            .upsert_user_profile(
                object(&[
                    ("type", JsValue::from("user")),
                    ("name", JsValue::from("user-profile")),
                    ("description", JsValue::from("Stable identity")),
                    ("profile", JsValue::from("Tan builds systems")),
                ])
                .as_object()
                .unwrap(),
            )
            .unwrap();
        let first = runtime
            .retrieve(MemoryRetrieveInput {
                query: "what do you know about me?".into(),
                session_id: "cache".into(),
                project_root: "/project".into(),
                recent_messages: Vec::new(),
                signal: Cancellation::default(),
            })
            .await
            .unwrap();
        let second = runtime
            .retrieve(MemoryRetrieveInput {
                query: "what do you know about me?".into(),
                session_id: "cache".into(),
                project_root: "/project".into(),
                recent_messages: Vec::new(),
                signal: Cancellation::default(),
            })
            .await
            .unwrap();
        assert_eq!(llm.calls.lock().unwrap().len(), 2);
        assert_eq!(first.system_context, second.system_context);
        assert_eq!(
            second
                .metadata
                .as_ref()
                .and_then(|value| value.get("debug"))
                .and_then(|value| value.get("cacheHit")),
            Some(&JsValue::Bool(true))
        );
    }

    #[tokio::test]
    async fn general_workspace_selects_one_readable_project_before_file_recall() {
        let scratch = crate::scratch::Scratch::new("native-edgeclaw-general-recall");
        let general = scratch.path().join("general");
        std::fs::create_dir_all(&general).unwrap();
        let llm = ScriptedLlm::new(vec![
            (
                "decide_file_memory_route",
                Ok(object(&[("route", JsValue::from("project"))])),
            ),
            (
                "memory_recall_project",
                Ok(object(&[
                    ("selected_project_id", JsValue::from("beta")),
                    ("reason", JsValue::from("The query names Beta.")),
                ])),
            ),
            (
                "select_file_manifest_entries",
                Ok(object(&[("selected_ids", JsValue::Array(Vec::new()))])),
            ),
        ]);
        let runtime = NativeEdgeClawRuntime::open(
            &general,
            NativeEdgeClawOptions::new(scratch.path().join("memory-root"), &general)
                .with_llm(llm.clone()),
        )
        .unwrap();
        for (id, name) in [("alpha", "Alpha"), ("beta", "Beta")] {
            runtime
                .inner
                .stores
                .workspace_memory
                .upsert_general_project_meta(
                    object(&[
                        ("projectId", JsValue::from(id)),
                        ("projectName", JsValue::from(name)),
                        ("description", string(format!("{name} project"))),
                    ])
                    .as_object()
                    .unwrap(),
                )
                .unwrap();
            runtime
                .inner
                .stores
                .workspace_memory
                .upsert_candidate(
                    object(&[
                        ("type", JsValue::from("project")),
                        ("scope", JsValue::from("project")),
                        ("projectId", JsValue::from(id)),
                        ("name", string(format!("{id}-fact"))),
                        ("description", string(format!("{name} fact"))),
                        ("body", string(format!("{name} private memory"))),
                    ])
                    .as_object()
                    .unwrap(),
                )
                .unwrap();
        }
        let result = runtime
            .retrieve(MemoryRetrieveInput {
                query: "What is the Beta status?".into(),
                session_id: "general".into(),
                project_root: general.to_string_lossy().into(),
                recent_messages: Vec::new(),
                signal: Cancellation::default(),
            })
            .await
            .unwrap();
        let context = result.system_context.unwrap();
        assert!(context.contains("Beta private memory"));
        assert!(!context.contains("Alpha private memory"));
        let calls = llm.calls.lock().unwrap();
        assert_eq!(calls[1].0, "memory_recall_project");
        let shortlist = calls[1]
            .1
            .get("shortlist")
            .and_then(|value| match value {
                JsValue::Array(values) => Some(values),
                _ => None,
            })
            .unwrap();
        assert_eq!(
            value_text(
                shortlist[0]
                    .get("project_id")
                    .or_else(|| shortlist[0].get("projectId"))
            )
            .as_deref(),
            Some("beta")
        );
    }

    #[tokio::test]
    async fn capture_maintenance_retrieve_and_case_trace_share_one_repository() {
        let llm = ScriptedLlm::new(vec![
            (
                "memory_classify_turn",
                Ok(object(&[
                    ("should_store", JsValue::Bool(true)),
                    (
                        "labels",
                        JsValue::Array(vec![object(&[("type", JsValue::from("project"))])]),
                    ),
                ])),
            ),
            (
                "memory_create_project",
                Ok(object(&[
                    ("name", JsValue::from("cobalt-launch")),
                    ("description", JsValue::from("Cobalt launch schedule")),
                    (
                        "body",
                        JsValue::from("Remember cobalt launch; launch is Friday."),
                    ),
                ])),
            ),
            (
                "decide_file_memory_route",
                Ok(object(&[("route", JsValue::from("project"))])),
            ),
            (
                "select_file_manifest_entries",
                Ok(object(&[("selected_ids", JsValue::Array(Vec::new()))])),
            ),
        ]);
        let (_scratch, runtime) = runtime_with(|options| {
            options
                .with_llm(llm)
                .with_indexing_settings(super::super::IndexingSettings {
                    reasoning_mode: "answer_first".into(),
                    auto_index_interval_minutes: 0,
                    auto_dream_interval_minutes: 60,
                })
        });
        runtime
            .capture_turn(MemoryCaptureTurnInput {
                session_id: "s1".into(),
                project_root: "/project".into(),
                messages: vec![
                    message("user", "ignore inherited", true),
                    message("user", "remember cobalt launch", false),
                    message("assistant", "launch is Friday", false),
                ],
                errored: false,
            })
            .await
            .unwrap();
        runtime
            .run_due_scheduled_maintenance("scheduled:test")
            .await
            .unwrap();

        let result = runtime
            .retrieve(MemoryRetrieveInput {
                query: "cobalt launch".into(),
                session_id: "s1".into(),
                project_root: "/project".into(),
                recent_messages: Vec::new(),
                signal: Cancellation::default(),
            })
            .await
            .unwrap();
        let context = result.system_context.as_deref().unwrap();
        assert!(context.contains("Remember cobalt launch"));
        assert!(!context.contains("ignore inherited"));
        assert!(context.starts_with("## ClawXMemory Recall"));
        assert!(context.contains("### [project"));

        runtime
            .capture_turn(MemoryCaptureTurnInput {
                session_id: "s1".into(),
                project_root: "/project".into(),
                messages: vec![message("assistant", "done", false)],
                errored: false,
            })
            .await
            .unwrap();
        let cases = runtime
            .inner
            .stores
            .storage
            .list_recent_case_traces(10)
            .unwrap();
        assert_eq!(cases.len(), 1);
        assert_eq!(cases[0].get("query"), Some(&JsValue::from("cobalt launch")));
    }

    #[tokio::test]
    async fn index_classifies_each_focus_turn_then_creates_each_labeled_memory_kind() {
        let llm = ScriptedLlm::new(vec![
            (
                "memory_classify_turn",
                Ok(object(&[
                    ("should_store", JsValue::Bool(true)),
                    (
                        "labels",
                        JsValue::Array(vec![
                            object(&[("type", JsValue::from("user"))]),
                            object(&[("type", JsValue::from("project"))]),
                        ]),
                    ),
                ])),
            ),
            (
                "memory_create_user",
                Ok(object(&[
                    ("name", JsValue::from("systems-role")),
                    ("description", JsValue::from("Stable professional role")),
                    (
                        "body",
                        JsValue::from("The user builds distributed systems."),
                    ),
                ])),
            ),
            (
                "memory_create_project",
                Ok(object(&[
                    ("name", JsValue::from("release-target")),
                    ("description", JsValue::from("Current release target")),
                    (
                        "body",
                        JsValue::from("The current release target is Friday."),
                    ),
                ])),
            ),
            (
                "memory_classify_turn",
                Ok(object(&[
                    ("should_store", JsValue::Bool(true)),
                    (
                        "labels",
                        JsValue::Array(vec![object(&[("type", JsValue::from("feedback"))])]),
                    ),
                ])),
            ),
            (
                "memory_create_feedback",
                Ok(object(&[
                    ("name", JsValue::from("status-format")),
                    ("description", JsValue::from("Status report format")),
                    ("body", JsValue::from("Report blockers before progress.")),
                ])),
            ),
        ]);
        let (_scratch, runtime) = runtime_with(|options| {
            options.with_llm(llm.clone()).with_capture_policy(
                CaptureStrategy::FullSession,
                true,
                6_000,
            )
        });
        runtime
            .capture_turn(MemoryCaptureTurnInput {
                session_id: "focus".into(),
                project_root: "/project".into(),
                messages: vec![
                    message(
                        "user",
                        "I build distributed systems and this release targets Friday.",
                        false,
                    ),
                    message("assistant", "Noted.", false),
                    message("user", "Report blockers before progress.", false),
                ],
                errored: false,
            })
            .await
            .unwrap();
        runtime.run_index_cycle("manual:test").await.unwrap();

        let kinds = runtime
            .list(100)
            .unwrap()
            .iter()
            .filter_map(|entry| value_text(entry.get("type")))
            .collect::<HashSet<_>>();
        assert!(kinds.contains("user"));
        assert!(kinds.contains("project"));
        assert!(kinds.contains("feedback"));
        let calls = llm.calls.lock().unwrap();
        assert_eq!(
            calls.iter().map(|call| call.0).collect::<Vec<_>>(),
            [
                "memory_classify_turn",
                "memory_create_user",
                "memory_create_project",
                "memory_classify_turn",
                "memory_create_feedback",
            ]
        );
        assert_eq!(
            value_text(
                calls[0]
                    .1
                    .get("focusUserTurn")
                    .and_then(|turn| turn.get("content"))
            )
            .as_deref(),
            Some("I build distributed systems and this release targets Friday.")
        );
        assert_eq!(
            value_text(
                calls[3]
                    .1
                    .get("focusUserTurn")
                    .and_then(|turn| turn.get("content"))
            )
            .as_deref(),
            Some("Report blockers before progress.")
        );
    }

    #[tokio::test]
    async fn general_index_routes_candidate_to_one_local_project_before_persisting() {
        let scratch = crate::scratch::Scratch::new("native-edgeclaw-general-index");
        let general = scratch.path().join("general");
        std::fs::create_dir_all(&general).unwrap();
        let llm = ScriptedLlm::new(vec![
            (
                "memory_classify_turn",
                Ok(object(&[
                    ("should_store", JsValue::Bool(true)),
                    (
                        "labels",
                        JsValue::Array(vec![object(&[("type", JsValue::from("project"))])]),
                    ),
                ])),
            ),
            (
                "memory_create_project",
                Ok(object(&[
                    ("name", JsValue::from("Beta release")),
                    ("description", JsValue::from("Beta launch plan")),
                    ("body", JsValue::from("Ship Beta after the final review.")),
                ])),
            ),
            (
                "memory_index_project",
                Ok(object(&[
                    ("decision", JsValue::from("attach_existing")),
                    ("selected_project_id", JsValue::from("beta")),
                    ("reason", JsValue::from("The candidate names Beta.")),
                ])),
            ),
        ]);
        let runtime = NativeEdgeClawRuntime::open(
            &general,
            NativeEdgeClawOptions::new(scratch.path().join("memory-root"), &general)
                .with_llm(llm.clone()),
        )
        .unwrap();
        for (id, name) in [("alpha", "Alpha"), ("beta", "Beta")] {
            runtime
                .inner
                .stores
                .workspace_memory
                .upsert_general_project_meta(
                    object(&[
                        ("projectId", JsValue::from(id)),
                        ("projectName", JsValue::from(name)),
                        ("description", string(format!("{name} project"))),
                    ])
                    .as_object()
                    .unwrap(),
                )
                .unwrap();
        }
        runtime
            .capture_turn(MemoryCaptureTurnInput {
                session_id: "general-index".into(),
                project_root: general.to_string_lossy().into(),
                messages: vec![message("user", "Update the Beta release plan", false)],
                errored: false,
            })
            .await
            .unwrap();
        runtime.run_index_cycle("manual").await.unwrap();

        let entries = runtime.inner.stores.list_memory_entries(100).unwrap();
        let stored = entries
            .iter()
            .find(|entry| value_text(entry.get("name")).as_deref() == Some("Beta release"))
            .expect("candidate persisted");
        assert_eq!(value_text(stored.get("projectId")).as_deref(), Some("beta"));
        let calls = llm.calls.lock().unwrap();
        assert_eq!(calls[2].0, "memory_index_project");
        assert!(
            calls[2]
                .1
                .get("shortlist")
                .and_then(|value| match value {
                    JsValue::Array(values) => Some(values.as_slice()),
                    _ => None,
                })
                .is_some_and(|values| values.iter().all(|entry| {
                    value_text(entry.get("sourceType")).as_deref() == Some("general_local")
                }))
        );
    }

    #[tokio::test]
    async fn general_index_route_failure_stays_pending_until_selection_retry_succeeds() {
        let classification = || {
            Ok(object(&[
                ("should_store", JsValue::Bool(true)),
                (
                    "labels",
                    JsValue::Array(vec![object(&[("type", JsValue::from("project"))])]),
                ),
            ]))
        };
        let candidate = || {
            Ok(object(&[
                ("name", JsValue::from("Beta release")),
                ("description", JsValue::from("Beta launch plan")),
                ("body", JsValue::from("Ship Beta after review.")),
            ]))
        };
        let llm = ScriptedLlm::new(vec![
            ("memory_classify_turn", classification()),
            ("memory_create_project", candidate()),
            ("memory_index_project", Err("selection unavailable".into())),
            ("memory_classify_turn", classification()),
            ("memory_create_project", candidate()),
            (
                "memory_index_project",
                Ok(object(&[
                    ("decision", JsValue::from("attach_existing")),
                    ("selected_project_id", JsValue::from("beta")),
                ])),
            ),
        ]);
        let scratch = crate::scratch::Scratch::new("native-edgeclaw-general-index-retry");
        let general = scratch.path().join("general");
        std::fs::create_dir_all(&general).unwrap();
        let runtime = NativeEdgeClawRuntime::open(
            &general,
            NativeEdgeClawOptions::new(scratch.path().join("memory-root"), &general).with_llm(llm),
        )
        .unwrap();
        runtime
            .inner
            .stores
            .workspace_memory
            .upsert_general_project_meta(
                object(&[
                    ("projectId", JsValue::from("beta")),
                    ("projectName", JsValue::from("Beta")),
                    ("description", JsValue::from("Beta project")),
                ])
                .as_object()
                .unwrap(),
            )
            .unwrap();
        runtime
            .capture_turn(MemoryCaptureTurnInput {
                session_id: "general-route-retry".into(),
                project_root: general.to_string_lossy().into(),
                messages: vec![message("user", "Update the Beta release plan", false)],
                errored: false,
            })
            .await
            .unwrap();

        runtime.run_index_cycle("manual:first").await.unwrap();
        assert_eq!(
            runtime
                .inner
                .stores
                .storage
                .count_pending_dialogue_turns(None)
                .unwrap(),
            1
        );
        let metas = runtime
            .inner
            .stores
            .workspace_memory
            .list_project_metas()
            .unwrap();
        assert!(matches!(metas, JsValue::Array(ref values) if values.len() == 1));
        assert!(
            !runtime
                .inner
                .stores
                .list_memory_entries(100)
                .unwrap()
                .iter()
                .any(|entry| value_text(entry.get("name")).as_deref() == Some("Beta release"))
        );

        runtime.run_index_cycle("manual:retry").await.unwrap();
        assert_eq!(
            runtime
                .inner
                .stores
                .storage
                .count_pending_dialogue_turns(None)
                .unwrap(),
            0
        );
        assert!(
            runtime
                .inner
                .stores
                .list_memory_entries(100)
                .unwrap()
                .iter()
                .any(|entry| value_text(entry.get("name")).as_deref() == Some("Beta release"))
        );
    }

    #[tokio::test]
    async fn capture_policy_filters_assistant_truncates_utf16_and_deduplicates_l0() {
        let (_scratch, runtime) = runtime_with(|options| {
            options.with_capture_policy(CaptureStrategy::FullSession, true, 2)
        });
        let input = MemoryCaptureTurnInput {
            session_id: "capture-policy".into(),
            project_root: "/project".into(),
            messages: vec![
                message("user", "abcdef", false),
                message("assistant", "<think>secret</think>😀x", false),
                message("user", "/help", false),
                message("assistant", "用户需要记录", false),
            ],
            errored: false,
        };
        runtime.capture_turn(input.clone()).await.unwrap();
        runtime.capture_turn(input).await.unwrap();

        let rows = runtime.inner.stores.storage.list_all_l0().unwrap();
        assert_eq!(
            rows.len(),
            1,
            "identical capture must not append a second L0"
        );
        let JsValue::Array(messages) = &rows[0].messages else {
            panic!("L0 messages must be an array")
        };
        assert_eq!(messages.len(), 2);
        assert_eq!(
            value_text(messages[0].get("content")).as_deref(),
            Some("ab...")
        );
        assert_eq!(
            value_text(messages[1].get("content")).as_deref(),
            Some("😀...")
        );
        assert!(rows[0].l0_index_id.starts_with("capture-policy_"));
        assert!(rows[0].l0_index_id.ends_with("_raw"));
    }

    #[tokio::test]
    async fn snapshot_factory_applies_capture_config_before_reload() {
        let scratch = crate::scratch::Scratch::new("native-edgeclaw-snapshot-policy");
        let project = scratch.path().join("project");
        let root = scratch.path().join("configured-memory");
        let home = scratch.path().join("home");
        std::fs::create_dir(&project).unwrap();
        let snapshot = object(&[(
            "config",
            object(&[(
                "memory",
                object(&[
                    ("enabled", JsValue::Bool(true)),
                    ("provider", JsValue::from("edgeclaw")),
                    ("rootDir", JsValue::from(root.to_string_lossy().as_ref())),
                    ("captureStrategy", JsValue::from("full_session")),
                    ("includeAssistant", JsValue::Bool(false)),
                    ("maxMessageChars", JsValue::Number(3.0)),
                    ("retrievalTimeoutMs", JsValue::Number(1_234.0)),
                ]),
            )]),
        )]);
        let owner = stage_native_from_snapshot(&project, &snapshot, &home, None)
            .unwrap()
            .unwrap();
        assert_eq!(owner.retrieval_timeout_ms, 1_234.0);
        owner
            .resolver
            .capture_turn(MemoryCaptureTurnInput {
                session_id: "factory".into(),
                project_root: project.to_string_lossy().into(),
                messages: vec![
                    message("user", "first", false),
                    message("assistant", "not stored", false),
                    message("user", "second", false),
                ],
                errored: false,
            })
            .await
            .unwrap();
        (owner.disposer)().await.unwrap();

        let reopened =
            NativeEdgeClawRuntime::open(&project, NativeEdgeClawOptions::new(&root, &home))
                .unwrap();
        let rows = reopened.inner.stores.storage.list_all_l0().unwrap();
        let JsValue::Array(messages) = &rows[0].messages else {
            panic!("L0 messages must be an array")
        };
        assert_eq!(messages.len(), 2);
        assert_eq!(
            value_text(messages[0].get("content")).as_deref(),
            Some("fir...")
        );
        assert_eq!(
            value_text(messages[1].get("content")).as_deref(),
            Some("sec...")
        );
    }

    #[tokio::test]
    async fn mixed_retrieval_uses_global_profile_and_project_scoped_files() {
        let llm = ScriptedLlm::new(Vec::new());
        let (_scratch, runtime) = runtime_with(|options| options.with_llm(llm.clone()));
        runtime
            .inner
            .stores
            .global_user_memory
            .upsert_user_profile(
                object(&[
                    ("type", JsValue::from("user")),
                    ("name", JsValue::from("user-profile")),
                    ("description", JsValue::from("Stable identity")),
                    ("profile", JsValue::from("Tan is a systems engineer")),
                ])
                .as_object()
                .unwrap(),
            )
            .unwrap();
        let global_note = runtime
            .inner
            .stores
            .global_user_memory
            .upsert_candidate(
                object(&[
                    ("type", JsValue::from("user")),
                    ("scope", JsValue::from("global")),
                    ("name", JsValue::from("private-note")),
                    (
                        "description",
                        JsValue::from("must not enter mixed file selection"),
                    ),
                    ("body", JsValue::from("GLOBAL NOTE MUST NOT BE INJECTED")),
                ])
                .as_object()
                .unwrap(),
            )
            .unwrap();
        let project = runtime
            .inner
            .stores
            .workspace_memory
            .upsert_candidate(
                object(&[
                    ("type", JsValue::from("project")),
                    ("scope", JsValue::from("project")),
                    ("name", JsValue::from("provider-parity")),
                    ("description", JsValue::from("Memory provider parity")),
                    ("body", JsValue::from("Project fact body")),
                ])
                .as_object()
                .unwrap(),
            )
            .unwrap();
        let feedback = runtime
            .inner
            .stores
            .workspace_memory
            .upsert_candidate(
                object(&[
                    ("type", JsValue::from("feedback")),
                    ("scope", JsValue::from("project")),
                    ("name", JsValue::from("report-rule")),
                    ("description", JsValue::from("Report exact state")),
                    ("body", JsValue::from("Feedback rule body")),
                ])
                .as_object()
                .unwrap(),
            )
            .unwrap();
        llm.responses.lock().unwrap().push_back((
            "decide_file_memory_route",
            Ok(object(&[("route", JsValue::from("mix"))])),
        ));
        llm.responses.lock().unwrap().push_back((
            "select_file_manifest_entries",
            Ok(object(&[(
                "selected_ids",
                JsValue::Array(vec![
                    global_note.get("relativePath").unwrap().clone(),
                    project.get("relativePath").unwrap().clone(),
                    feedback.get("relativePath").unwrap().clone(),
                ]),
            )])),
        ));

        let result = runtime
            .retrieve(MemoryRetrieveInput {
                query: "What do we know about me and this project?".into(),
                session_id: "mixed".into(),
                project_root: "/project".into(),
                recent_messages: vec![message("user", "parent context", true)],
                signal: Cancellation::default(),
            })
            .await
            .unwrap();
        let context = result.system_context.unwrap();
        assert!(context.contains("route=mix"));
        assert!(context.contains("### [user]"));
        assert!(context.contains("Tan is a systems engineer"));
        assert!(context.contains("## 身份背景"));
        assert!(context.contains("Project fact body"));
        assert!(context.contains("Feedback rule body"));
        assert!(!context.contains("GLOBAL NOTE MUST NOT BE INJECTED"));
        assert!(context.contains("Treat these file memories as the authoritative long-term memory for this turn when relevant."));
        let calls = llm.calls.lock().unwrap();
        let recent = calls[0]
            .1
            .get("recent_messages")
            .or_else(|| calls[0].1.get("recentMessages"))
            .and_then(|value| match value {
                JsValue::Array(values) => Some(values),
                _ => None,
            })
            .unwrap();
        assert_eq!(recent.len(), 1, "retrieval retains truthy fork carryover");
    }

    #[tokio::test]
    async fn classification_failure_keeps_l0_pending_until_a_later_success() {
        let llm = ScriptedLlm::new(vec![
            (
                "memory_classify_turn",
                Err("temporary model failure".into()),
            ),
            (
                "memory_classify_turn",
                Ok(object(&[
                    ("should_store", JsValue::Bool(false)),
                    ("labels", JsValue::Array(Vec::new())),
                ])),
            ),
        ]);
        let (_scratch, runtime) = runtime_with(|options| options.with_llm(llm));
        runtime
            .capture_turn(MemoryCaptureTurnInput {
                session_id: "retry".into(),
                project_root: "/project".into(),
                messages: vec![message("user", "remember this", false)],
                errored: false,
            })
            .await
            .unwrap();

        runtime.run_index_cycle("scheduled:test").await.unwrap();
        assert_eq!(
            runtime
                .inner
                .stores
                .storage
                .count_pending_dialogue_turns(None)
                .unwrap(),
            1
        );
        assert!(
            runtime
                .inner
                .stores
                .storage
                .get_pipeline_state("lastIndexedAt")
                .unwrap()
                .is_none()
        );
        runtime.run_index_cycle("scheduled:retry").await.unwrap();
        assert_eq!(
            runtime
                .inner
                .stores
                .storage
                .count_pending_dialogue_turns(None)
                .unwrap(),
            0
        );
        let traces = runtime
            .inner
            .stores
            .storage
            .list_recent_index_traces(10)
            .unwrap();
        assert_eq!(traces.len(), 2);
        assert!(
            traces
                .iter()
                .any(|trace| { trace.get("status") == Some(&JsValue::from("error")) })
        );
        assert!(
            traces
                .iter()
                .any(|trace| { trace.get("status") == Some(&JsValue::from("completed")) })
        );
    }

    #[tokio::test]
    async fn create_failure_keeps_l0_pending_until_candidate_persists() {
        let classification = || {
            Ok(object(&[
                ("should_store", JsValue::Bool(true)),
                (
                    "labels",
                    JsValue::Array(vec![object(&[("type", JsValue::from("project"))])]),
                ),
            ]))
        };
        let llm = ScriptedLlm::new(vec![
            ("memory_classify_turn", classification()),
            (
                "memory_create_project",
                Err("temporary create failure".into()),
            ),
            ("memory_classify_turn", classification()),
            (
                "memory_create_project",
                Ok(object(&[
                    ("name", JsValue::from("Retry candidate")),
                    ("description", JsValue::from("Created after retry")),
                    ("body", JsValue::from("Durable retry evidence.")),
                ])),
            ),
        ]);
        let (_scratch, runtime) = runtime_with(|options| options.with_llm(llm));
        runtime
            .capture_turn(MemoryCaptureTurnInput {
                session_id: "create-retry".into(),
                project_root: "/project".into(),
                messages: vec![message("user", "remember the retry evidence", false)],
                errored: false,
            })
            .await
            .unwrap();

        runtime.run_index_cycle("manual:first").await.unwrap();
        assert_eq!(
            runtime
                .inner
                .stores
                .storage
                .count_pending_dialogue_turns(None)
                .unwrap(),
            1
        );
        runtime.run_index_cycle("manual:retry").await.unwrap();
        assert_eq!(
            runtime
                .inner
                .stores
                .storage
                .count_pending_dialogue_turns(None)
                .unwrap(),
            0
        );
        assert!(
            runtime
                .inner
                .stores
                .list_memory_entries(100)
                .unwrap()
                .iter()
                .any(|entry| value_text(entry.get("name")).as_deref() == Some("Retry candidate"))
        );
    }

    #[tokio::test]
    async fn second_label_failure_does_not_partially_persist_first_candidate() {
        let classification = || {
            Ok(object(&[
                ("should_store", JsValue::Bool(true)),
                (
                    "labels",
                    JsValue::Array(vec![
                        object(&[("type", JsValue::from("user"))]),
                        object(&[("type", JsValue::from("project"))]),
                    ]),
                ),
            ]))
        };
        let user_candidate = || {
            Ok(object(&[
                ("name", JsValue::from("User preference")),
                ("description", JsValue::from("Stable preference")),
                ("body", JsValue::from("Prefers concise status reports.")),
            ]))
        };
        let project_candidate = || {
            Ok(object(&[
                ("name", JsValue::from("Project decision")),
                ("description", JsValue::from("Stable project decision")),
                ("body", JsValue::from("Ship after the review gate.")),
            ]))
        };
        let llm = ScriptedLlm::new(vec![
            ("memory_classify_turn", classification()),
            ("memory_create_user", user_candidate()),
            ("memory_create_project", Err("second label failed".into())),
            ("memory_classify_turn", classification()),
            ("memory_create_user", user_candidate()),
            ("memory_create_project", project_candidate()),
        ]);
        let (_scratch, runtime) = runtime_with(|options| options.with_llm(llm));
        runtime
            .capture_turn(MemoryCaptureTurnInput {
                session_id: "multi-label-retry".into(),
                project_root: "/project".into(),
                messages: vec![message("user", "remember both durable facts", false)],
                errored: false,
            })
            .await
            .unwrap();

        runtime.run_index_cycle("manual:first").await.unwrap();
        assert!(
            runtime
                .inner
                .stores
                .list_memory_entries(100)
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            runtime
                .inner
                .stores
                .storage
                .count_pending_dialogue_turns(None)
                .unwrap(),
            1
        );

        runtime.run_index_cycle("manual:retry").await.unwrap();
        let entries = runtime.inner.stores.list_memory_entries(100).unwrap();
        assert_eq!(
            entries
                .iter()
                .filter(|entry| matches!(
                    value_text(entry.get("name")).as_deref(),
                    Some("User preference" | "Project decision")
                ))
                .count(),
            2
        );
        assert_eq!(
            runtime
                .inner
                .stores
                .storage
                .count_pending_dialogue_turns(None)
                .unwrap(),
            0
        );
    }

    #[tokio::test]
    async fn cumulative_l0_snapshots_only_classify_new_user_turns() {
        let now = Arc::new(Mutex::new("2026-10-31T00:00:00.000Z".to_owned()));
        let llm = ScriptedLlm::new(vec![
            (
                "memory_classify_turn",
                Ok(object(&[
                    ("should_store", JsValue::Bool(false)),
                    ("labels", JsValue::Array(Vec::new())),
                ])),
            ),
            (
                "memory_classify_turn",
                Ok(object(&[
                    ("should_store", JsValue::Bool(false)),
                    ("labels", JsValue::Array(Vec::new())),
                ])),
            ),
        ]);
        let clock_now = Arc::clone(&now);
        let (_scratch, runtime) = runtime_with(|options| {
            options
                .with_llm(llm.clone())
                .with_capture_policy(CaptureStrategy::FullSession, true, 6_000)
                .with_clock(Arc::new(move || clock_now.lock().unwrap().clone()))
        });
        runtime
            .capture_turn(MemoryCaptureTurnInput {
                session_id: "cumulative".into(),
                project_root: "/project".into(),
                messages: vec![message("user", "first request", false)],
                errored: false,
            })
            .await
            .unwrap();
        runtime.run_index_cycle("manual:first").await.unwrap();

        *now.lock().unwrap() = "2026-10-31T00:01:00.000Z".to_owned();
        runtime
            .capture_turn(MemoryCaptureTurnInput {
                session_id: "cumulative".into(),
                project_root: "/project".into(),
                messages: vec![
                    message("user", "first request", false),
                    message("assistant", "first response", false),
                    message("user", "second request", false),
                ],
                errored: false,
            })
            .await
            .unwrap();
        runtime.run_index_cycle("manual:second").await.unwrap();

        let calls = llm.calls.lock().unwrap();
        assert_eq!(calls.len(), 2);
        assert_eq!(
            value_text(
                calls[0]
                    .1
                    .get("focusUserTurn")
                    .and_then(|value| value.get("content"))
            )
            .as_deref(),
            Some("first request")
        );
        assert_eq!(
            value_text(
                calls[1]
                    .1
                    .get("focusUserTurn")
                    .and_then(|value| value.get("content"))
            )
            .as_deref(),
            Some("second request")
        );
    }

    #[tokio::test]
    async fn scheduled_heartbeat_waits_for_anchor_and_clears_it_after_indexing() {
        let now = Arc::new(Mutex::new("2026-10-31T00:00:00.000Z".to_owned()));
        let llm = ScriptedLlm::new(vec![(
            "memory_classify_turn",
            Ok(object(&[
                ("should_store", JsValue::Bool(false)),
                ("labels", JsValue::Array(Vec::new())),
            ])),
        )]);
        let clock_now = Arc::clone(&now);
        let (_scratch, runtime) = runtime_with(|options| {
            options
                .with_llm(llm.clone())
                .with_clock(Arc::new(move || clock_now.lock().unwrap().clone()))
                .with_indexing_settings(super::super::IndexingSettings {
                    reasoning_mode: "answer_first".into(),
                    auto_index_interval_minutes: 30,
                    auto_dream_interval_minutes: 60,
                })
        });
        runtime
            .capture_turn(MemoryCaptureTurnInput {
                session_id: "scheduled".into(),
                project_root: "/project".into(),
                messages: vec![message("user", "wait for interval", false)],
                errored: false,
            })
            .await
            .unwrap();

        *now.lock().unwrap() = "2026-10-31T00:29:00.000Z".into();
        runtime
            .run_due_scheduled_maintenance("not-prefixed-scheduled")
            .await
            .unwrap();
        assert!(llm.calls.lock().unwrap().is_empty());
        assert_eq!(
            runtime
                .inner
                .stores
                .storage
                .count_pending_dialogue_turns(None)
                .unwrap(),
            1
        );

        *now.lock().unwrap() = "2026-10-31T00:30:00.000Z".into();
        runtime
            .run_due_scheduled_maintenance("scheduled:test")
            .await
            .unwrap();
        assert_eq!(llm.calls.lock().unwrap().len(), 1);
        assert_eq!(
            runtime
                .inner
                .stores
                .storage
                .count_pending_dialogue_turns(None)
                .unwrap(),
            0
        );
        assert!(
            runtime
                .inner
                .stores
                .storage
                .get_pipeline_state("autoIndexAnchorAt")
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn failed_dream_is_observational_and_disposes_staging_roots() {
        let llm = ScriptedLlm::new(vec![(
            "memory_dream_cluster_plan",
            Err("dream model failure".into()),
        )]);
        let (scratch, runtime) = runtime_with(|options| options.with_llm(llm));
        for i in 0..2 {
            runtime
                .inner
                .stores
                .workspace_memory
                .upsert_candidate(
                    object(&[
                        ("type", JsValue::from("project")),
                        ("scope", JsValue::from("project")),
                        ("name", string(format!("fixture-{i}"))),
                        ("description", string(format!("fixture-{i}"))),
                        ("body", string(format!("fixture body {i}"))),
                    ])
                    .as_object()
                    .unwrap(),
                )
                .unwrap();
        }
        let result = runtime.run_dream_cycle("manual").await;
        assert!(result.is_err());
        assert_eq!(runtime.list(100).unwrap().len(), 2);
        let traces = runtime
            .inner
            .stores
            .storage
            .list_recent_dream_traces(10)
            .unwrap();
        assert_eq!(traces.len(), 1);
        assert_eq!(traces[0].get("status"), Some(&JsValue::from("error")));

        let mut pending = vec![scratch.path().to_path_buf()];
        while let Some(path) = pending.pop() {
            for entry in std::fs::read_dir(path).unwrap() {
                let entry = entry.unwrap();
                if entry.file_type().unwrap().is_dir() {
                    pending.push(entry.path());
                }
                assert!(
                    !entry.file_name().to_string_lossy().contains(".dream-"),
                    "failed dream left a staging path: {}",
                    entry.path().display()
                );
            }
        }
    }

    #[tokio::test]
    async fn scheduled_dream_uses_index_anchor_and_commits_pipeline_state() {
        let now = Arc::new(Mutex::new("2026-10-31T00:00:00.000Z".to_owned()));
        let llm = ScriptedLlm::new(vec![(
            "memory_dream_project_meta_review",
            Ok(object(&[
                ("should_update", JsValue::Bool(false)),
                ("summary", JsValue::from("No memory changes required.")),
            ])),
        )]);
        let clock_now = Arc::clone(&now);
        let (_scratch, runtime) = runtime_with(|options| {
            options
                .with_llm(llm.clone())
                .with_clock(Arc::new(move || clock_now.lock().unwrap().clone()))
                .with_indexing_settings(super::super::IndexingSettings {
                    reasoning_mode: "answer_first".into(),
                    auto_index_interval_minutes: 30,
                    auto_dream_interval_minutes: 30,
                })
        });
        runtime
            .inner
            .stores
            .workspace_memory
            .upsert_candidate(
                object(&[
                    ("type", JsValue::from("project")),
                    ("scope", JsValue::from("project")),
                    ("name", JsValue::from("dream-anchor")),
                    ("description", JsValue::from("Dream anchor fixture")),
                    ("body", JsValue::from("Fixture memory")),
                ])
                .as_object()
                .unwrap(),
            )
            .unwrap();
        runtime
            .inner
            .stores
            .storage
            .set_pipeline_state("lastIndexedAt", &JsValue::from("2026-10-31T00:00:00.000Z"))
            .unwrap();

        *now.lock().unwrap() = "2026-10-31T00:29:00.000Z".into();
        runtime
            .run_due_scheduled_maintenance("scheduled:test")
            .await
            .unwrap();
        assert!(llm.calls.lock().unwrap().is_empty());

        *now.lock().unwrap() = "2026-10-31T00:30:00.000Z".into();
        runtime
            .run_due_scheduled_maintenance("scheduled:test")
            .await
            .unwrap();
        assert_eq!(llm.calls.lock().unwrap().len(), 1);
        assert_eq!(
            runtime
                .inner
                .stores
                .storage
                .get_pipeline_state("lastDreamSummary")
                .unwrap(),
            Some(JsValue::from("Memory dream completed."))
        );
        assert!(
            runtime
                .inner
                .stores
                .storage
                .get_pipeline_state("autoDreamAnchorAt")
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn dream_plans_then_rewrites_project_feedback_and_user_profile() {
        let llm = ScriptedLlm::new(Vec::new());
        let (_scratch, runtime) = runtime_with(|options| options.with_llm(llm.clone()));
        for (kind, name, body) in [
            ("project", "project-old-a", "Old project A"),
            ("project", "project-old-b", "Old project B"),
            ("feedback", "feedback-old-a", "Old feedback A"),
            ("feedback", "feedback-old-b", "Old feedback B"),
        ] {
            runtime
                .inner
                .stores
                .workspace_memory
                .upsert_candidate(
                    object(&[
                        ("type", JsValue::from(kind)),
                        ("scope", JsValue::from("project")),
                        ("name", JsValue::from(name)),
                        ("description", JsValue::from(name)),
                        ("body", JsValue::from(body)),
                    ])
                    .as_object()
                    .unwrap(),
                )
                .unwrap();
        }
        runtime
            .inner
            .stores
            .global_user_memory
            .upsert_user_profile(
                object(&[
                    ("type", JsValue::from("user")),
                    ("scope", JsValue::from("global")),
                    ("name", JsValue::from("user-profile")),
                    ("description", JsValue::from("Existing user profile")),
                    ("profile", JsValue::from("Existing stable identity.")),
                ])
                .as_object()
                .unwrap(),
            )
            .unwrap();
        runtime
            .inner
            .stores
            .global_user_memory
            .upsert_candidate(
                object(&[
                    ("type", JsValue::from("user")),
                    ("scope", JsValue::from("global")),
                    ("name", JsValue::from("user-note")),
                    ("description", JsValue::from("User identity note")),
                    ("body", JsValue::from("Tan builds distributed systems.")),
                ])
                .as_object()
                .unwrap(),
            )
            .unwrap();
        let manifest = runtime.inner.stores.list_memory_entries(100).unwrap();
        let ids = |kind: &str| {
            manifest
                .iter()
                .filter(|entry| value_text(entry.get("type")).as_deref() == Some(kind))
                .filter_map(|entry| entry.get("relativePath"))
                .filter_map(JsValue::as_string)
                .cloned()
                .collect::<Vec<_>>()
        };
        let project_ids = ids("project");
        let feedback_ids = ids("feedback");
        let _user_ids = ids("user");
        llm.responses.lock().unwrap().extend([
            (
                "memory_dream_cluster_plan",
                Ok(object(&[
                    (
                        "summary",
                        JsValue::from("Rewrite duplicate project memories."),
                    ),
                    (
                        "project_clusters",
                        JsValue::Array(vec![object(&[
                            (
                                "member_relative_paths",
                                JsValue::Array(
                                    project_ids.iter().cloned().map(JsValue::String).collect(),
                                ),
                            ),
                            ("reason", JsValue::from("Duplicate project facts.")),
                        ])]),
                    ),
                ])),
            ),
            (
                "memory_dream_project_rewrite",
                Ok(object(&[
                    (
                        "candidates",
                        JsValue::Array(vec![object(&[
                            ("name", JsValue::from("project-rewritten")),
                            ("description", JsValue::from("Rewritten project memory")),
                            ("body", JsValue::from("Canonical project memory.")),
                        ])]),
                    ),
                    (
                        "delete_ids",
                        JsValue::Array(project_ids.iter().cloned().map(JsValue::String).collect()),
                    ),
                ])),
            ),
            (
                "memory_dream_cluster_plan",
                Ok(object(&[
                    (
                        "summary",
                        JsValue::from("Rewrite duplicate feedback memories."),
                    ),
                    (
                        "feedback_clusters",
                        JsValue::Array(vec![object(&[
                            (
                                "member_relative_paths",
                                JsValue::Array(
                                    feedback_ids.iter().cloned().map(JsValue::String).collect(),
                                ),
                            ),
                            ("reason", JsValue::from("Overlapping feedback rules.")),
                        ])]),
                    ),
                ])),
            ),
            (
                "memory_dream_feedback_rewrite",
                Ok(object(&[
                    (
                        "candidates",
                        JsValue::Array(vec![object(&[
                            ("name", JsValue::from("feedback-rewritten")),
                            ("description", JsValue::from("Rewritten feedback memory")),
                            ("body", JsValue::from("Canonical feedback memory.")),
                        ])]),
                    ),
                    (
                        "delete_ids",
                        JsValue::Array(feedback_ids.iter().cloned().map(JsValue::String).collect()),
                    ),
                ])),
            ),
            (
                "memory_dream_project_meta_review",
                Ok(object(&[("should_update", JsValue::Bool(false))])),
            ),
            (
                "memory_dream_user_profile_merge",
                Ok(object(&[(
                    "candidate",
                    object(&[
                        ("name", JsValue::from("user-profile")),
                        ("description", JsValue::from("Stable user identity")),
                        (
                            "profile",
                            JsValue::from("Tan is a distributed systems engineer."),
                        ),
                    ]),
                )])),
            ),
        ]);

        runtime.run_dream_cycle("manual").await.unwrap();
        let entries = runtime.list(100).unwrap();
        let names = entries
            .iter()
            .filter_map(|entry| value_text(entry.get("name")))
            .collect::<HashSet<_>>();
        assert!(names.contains("project-rewritten"));
        assert!(names.contains("feedback-rewritten"));
        assert!(!names.contains("project-old-a"));
        assert!(!names.contains("feedback-old-a"));
        let user_summary = runtime.inner.stores.get_user_summary().unwrap();
        assert!(
            user_summary
                .get("identityBackground")
                .and_then(|value| match value {
                    JsValue::Array(values) => Some(values),
                    _ => None,
                })
                .is_some_and(|items| items.iter().any(|item| {
                    value_text(Some(item)).is_some_and(|text| text.contains("distributed systems"))
                }))
        );
        assert_eq!(
            llm.calls
                .lock()
                .unwrap()
                .iter()
                .map(|call| call.0)
                .collect::<Vec<_>>(),
            [
                "memory_dream_cluster_plan",
                "memory_dream_project_rewrite",
                "memory_dream_cluster_plan",
                "memory_dream_feedback_rewrite",
                "memory_dream_project_meta_review",
                "memory_dream_user_profile_merge",
            ]
        );
    }

    #[tokio::test]
    async fn concurrent_maintenance_requests_merge_and_close_is_idempotent() {
        let (scratch, runtime) = runtime();
        runtime
            .capture_turn(MemoryCaptureTurnInput {
                session_id: "persisted".into(),
                project_root: "/project".into(),
                messages: vec![message("user", "survive reload", false)],
                errored: false,
            })
            .await
            .unwrap();
        let left = runtime.clone();
        let right = runtime.clone();
        let (a, b) = tokio::join!(
            async move { left.run_due_scheduled_maintenance("a").await },
            async move { right.run_due_scheduled_maintenance("b").await }
        );
        a.unwrap();
        b.unwrap();
        runtime.close().unwrap();
        runtime.close().unwrap();
        let closed = runtime
            .retrieve(MemoryRetrieveInput {
                query: "x".into(),
                session_id: "s".into(),
                project_root: "/project".into(),
                recent_messages: Vec::new(),
                signal: Cancellation::default(),
            })
            .await
            .unwrap();
        assert_eq!(closed.diagnostics[0].code, "memory_provider_error");

        let reopened = NativeEdgeClawRuntime::open(
            scratch.path().join("project"),
            NativeEdgeClawOptions::new(
                scratch.path().join("memory-root"),
                scratch.path().join("general"),
            ),
        )
        .unwrap();
        let rows = reopened.inner.stores.storage.list_all_l0().unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].session_key, "persisted");
    }

    #[tokio::test]
    async fn route_llm_failure_falls_back_to_none_and_returns_empty_recall_without_error() {
        // Aligns with TS llm-extraction.ts:2911-2913: LLM error must not bubble up as Err
        let llm = ScriptedLlm::new(vec![(
            "decide_file_memory_route",
            Err("network timeout to model provider".into()),
        )]);
        let (_scratch, runtime) = runtime_with(|options| options.with_llm(llm));
        let outcome = runtime
            .retrieve(MemoryRetrieveInput {
                query: "my preference".into(),
                session_id: "test".into(),
                project_root: "/project".into(),
                recent_messages: Vec::new(),
                signal: Cancellation::default(),
            })
            .await
            .expect("retrieval must succeed with empty recall on route failure");

        assert!(outcome.system_context.is_none());
        assert_eq!(outcome.diagnostics.len(), 1);
        assert_eq!(outcome.diagnostics[0].code, "memory_context_empty");
        let trace = outcome.metadata.unwrap().get("trace").cloned().unwrap();
        assert_eq!(trace.get("route"), Some(&JsValue::from("none")));
        let steps = match trace.get("steps") {
            Some(JsValue::Array(steps)) => steps,
            _ => panic!("expected array for steps"),
        };
        assert_eq!(steps[0].get("kind"), Some(&JsValue::from("recall_start")));
        let gate_step = steps
            .iter()
            .find(|s| value_text(s.get("kind")).as_deref() == Some("memory_gate"))
            .expect("memory_gate step must exist");
        assert_eq!(gate_step.get("status"), Some(&JsValue::from("warning")));
        assert!(gate_step.get("stepId").is_some());
        assert!(gate_step.get("promptDebug").is_some());
    }

    #[tokio::test]
    async fn independent_route_and_selection_calls_and_fallback_limit_3() {
        // Aligns with TS reasoning-loop.ts: fallbackSelection uses 1 for user, 3 for other routes
        let llm = ScriptedLlm::new(vec![
            (
                "decide_file_memory_route",
                Ok(object(&[("route", JsValue::from("project"))])),
            ),
            (
                "select_file_manifest_entries",
                Ok(object(&[("selected_ids", JsValue::Array(Vec::new()))])),
            ),
        ]);
        let (_scratch, runtime) = runtime_with(|options| options.with_llm(llm.clone()));
        // Insert 5 files in project memory
        for i in 1..=5 {
            let note = object(&[
                ("type", JsValue::from("project")),
                ("name", string(format!("file-{i}"))),
                ("description", string(format!("Description {i}"))),
                (
                    "body",
                    string(format!("Line 1 for file {i}\nLine 2 for file {i}")),
                ),
            ]);
            runtime
                .inner
                .stores
                .workspace_memory
                .upsert_candidate(note.as_object().unwrap())
                .unwrap();
        }

        let outcome = runtime
            .retrieve(MemoryRetrieveInput {
                query: "show project details".into(),
                session_id: "test".into(),
                project_root: "/project".into(),
                recent_messages: Vec::new(),
                signal: Cancellation::default(),
            })
            .await
            .unwrap();

        assert!(outcome.system_context.is_some());
        let trace = outcome.metadata.unwrap().get("trace").cloned().unwrap();
        assert_eq!(trace.get("route"), Some(&JsValue::from("project")));
        let selected = match trace.get("selectedIds") {
            Some(JsValue::Array(selected)) => selected,
            _ => panic!("expected array for selectedIds"),
        };
        // Fallback for non-user route must select strictly 3 files (not 5)
        assert_eq!(selected.len(), 3);

        let steps = match trace.get("steps") {
            Some(JsValue::Array(steps)) => steps,
            _ => panic!("expected array for steps"),
        };
        assert!(steps.len() >= 6);
        let kinds = steps
            .iter()
            .filter_map(|s| value_text(s.get("kind")))
            .collect::<Vec<_>>();
        assert!(kinds.contains(&"recall_start".to_string()));
        assert!(kinds.contains(&"memory_gate".to_string()));
        assert!(kinds.contains(&"manifest_scanned".to_string()));
        assert!(kinds.contains(&"manifest_selected".to_string()));
        assert!(kinds.contains(&"files_loaded".to_string()));
        assert!(kinds.contains(&"context_rendered".to_string()));

        for step in steps {
            assert!(step.get("stepId").is_some());
            assert!(step.get("title").is_some());
            assert!(
                step.get("titleI18n").is_some(),
                "step missing titleI18n: {:?}",
                step
            );
            assert!(step.get("status").is_some());
            assert!(step.get("inputSummary").is_some());
            assert!(step.get("outputSummary").is_some());
        }
    }

    #[tokio::test]
    async fn selection_truncates_to_limit_and_rich_trace_debug_local_fallback() {
        // Test that if LLM returns more IDs than limit (e.g. 3 for user route), it's truncated to limit (1)
        // Also verify local_fallback mode and warning status on context_rendered when no context was rendered.
        let llm = ScriptedLlm::new(vec![
            (
                "decide_file_memory_route",
                Ok(object(&[("route", JsValue::from("user"))])),
            ),
            (
                "select_file_manifest_entries",
                Ok(object(&[(
                    "selected_ids",
                    JsValue::Array(vec![
                        string("non-existent-1.md"),
                        string("non-existent-2.md"),
                        string("non-existent-3.md"),
                    ]),
                )])),
            ),
        ]);
        let (_scratch, runtime) = runtime_with(|options| options.with_llm(llm.clone()));
        for i in 1..=3 {
            let note = object(&[
                ("type", JsValue::from("user")),
                ("name", string(format!("pref-{i}"))),
                ("description", string(format!("User preference {i}"))),
                ("body", string(format!("Profile detail {i}"))),
            ]);
            runtime
                .inner
                .stores
                .global_user_memory
                .upsert_candidate(note.as_object().unwrap())
                .unwrap();
        }

        let outcome = runtime
            .retrieve(MemoryRetrieveInput {
                query: "who am i".into(),
                session_id: "test-user".into(),
                project_root: "/project".into(),
                recent_messages: Vec::new(),
                signal: Cancellation::default(),
            })
            .await
            .unwrap();

        let trace = outcome
            .metadata
            .as_ref()
            .unwrap()
            .get("trace")
            .cloned()
            .unwrap();
        let selected = match trace.get("selectedIds") {
            Some(JsValue::Array(selected)) => selected,
            _ => panic!("expected array for selectedIds"),
        };
        // User route limit is 1, so returned 3 must be truncated to 1
        assert_eq!(selected.len(), 1);

        // When non-existent file yields no context, TS specifies local_fallback and warning on context_rendered
        let debug = outcome
            .metadata
            .as_ref()
            .unwrap()
            .get("debug")
            .cloned()
            .unwrap();
        assert_eq!(debug.get("mode"), Some(&JsValue::from("local_fallback")));
        assert!(debug.get("elapsedMs").is_some());

        let steps = match trace.get("steps") {
            Some(JsValue::Array(steps)) => steps,
            _ => panic!("expected array for steps"),
        };
        let context_step = steps
            .iter()
            .find(|s| value_text(s.get("kind")).as_deref() == Some("context_rendered"))
            .expect("context_rendered step must exist");
        assert_eq!(context_step.get("status"), Some(&JsValue::from("warning")));
    }

    #[tokio::test]
    async fn retrieval_cancellation_aborts_immediately() {
        let (_scratch, runtime) = runtime();
        let cancellation = Cancellation::default();
        cancellation.cancel(None);
        let result = runtime
            .retrieve_now(&MemoryRetrieveInput {
                query: "cancelled".into(),
                session_id: "test".into(),
                project_root: "/project".into(),
                recent_messages: Vec::new(),
                signal: cancellation,
            })
            .await;
        assert_eq!(result.unwrap_err(), "Memory retrieval aborted.");
    }

    #[tokio::test]
    async fn ts_reasoning_loop_parity_render_context_and_empty_debug_path() {
        // 1. Verify truncate_for_prompt whitespace normalization and truncation
        assert_eq!(
            truncate_for_prompt("  hello \n\t world  ", 20),
            "hello world"
        );
        assert_eq!(
            truncate_for_prompt("hello    beautiful   world", 10),
            "hello beau"
        );
        assert_eq!(
            truncate_for_prompt("hello    beautiful   world", 5),
            "hello"
        );

        // 2. Verify render_system_context format matches TS reasoning-loop.ts:163-208
        let user_summary = object(&[
            (
                "files",
                JsValue::Array(vec![object(&[
                    (
                        "relativePath",
                        string("global/UserIdentity/user-profile.md"),
                    ),
                    ("updatedAt", string("2026-11-02")),
                ])]),
            ),
            (
                "identityBackground",
                JsValue::Array(vec![string("AI Engineer")]),
            ),
        ]);
        let project_meta = object(&[
            ("relativePath", string("projects/project.md")),
            ("updatedAt", string("2026-11-02")),
            ("projectName", string("PilotDeck")),
            ("description", string("PilotDeck standalone project")),
            ("status", string("active")),
        ]);
        let records = vec![object(&[
            ("relativePath", string("projects/arch.md")),
            ("type", string("project")),
            ("updatedAt", string("2026-11-02")),
            ("content", string("Architecture note content")),
        ])];

        let rendered =
            render_system_context("mix", Some(&user_summary), Some(&project_meta), &records);
        assert!(rendered.starts_with("## ClawXMemory Recall\nroute=mix\n"));
        assert!(rendered.contains("### [user] global/UserIdentity/user-profile.md (2026-11-02)"));
        assert!(rendered.contains("## 身份背景\n- AI Engineer"));
        assert!(rendered.contains("### [project_meta] projects/project.md (2026-11-02)"));
        assert!(rendered.contains("## Project Name\nPilotDeck"));
        assert!(rendered.contains("### [project] projects/arch.md (2026-11-02)"));
        assert!(rendered.contains("Architecture note content"));
        assert!(rendered.ends_with("Treat these file memories as the authoritative long-term memory for this turn when relevant."));

        // 3. Verify empty recall debug.path is "explicit"
        let llm = ScriptedLlm::new(vec![(
            "decide_file_memory_route",
            Ok(object(&[("route", JsValue::from("none"))])),
        )]);
        let (_scratch, runtime) = runtime_with(|options| options.with_llm(llm));
        let outcome = runtime
            .retrieve(MemoryRetrieveInput {
                query: "nothing".into(),
                session_id: "test".into(),
                project_root: "/project".into(),
                recent_messages: Vec::new(),
                signal: Cancellation::default(),
            })
            .await
            .expect("must succeed with empty recall");

        let debug = outcome.metadata.unwrap().get("debug").cloned().unwrap();
        assert_eq!(debug.get("mode"), Some(&JsValue::from("none")));
        assert_eq!(debug.get("path"), Some(&JsValue::from("explicit")));
        assert_eq!(debug.get("route"), Some(&JsValue::from("none")));
        assert_eq!(debug.get("manifestCount"), Some(&JsValue::Number(0.0)));
    }
}
