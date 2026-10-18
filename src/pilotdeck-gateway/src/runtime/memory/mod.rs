//! Project-scoped memory provider composition and management projection.
//!
//! Ports `ProjectMemoryBundle.ts:41-86` (stage/dispose and injected provider
//! ownership) and `createLocalGateway.ts:1238-1259` (`memory_list` /
//! `memory_wipe`). The native EdgeClaw service is intentionally not ported:
//! it needs SQLite, which is a separate decision (A6.6-pr10+). Callers inject
//! a provider factory; the factory returns `None` for the disabled or
//! unsupported configuration that the native factory used to report.
use crate::{
    metadata::js_trim,
    runtime::project::{DisposeFuture, RuntimeError, RuntimeResources},
};
use pilotdeck_protocol::{JsObject, JsValue};
use std::{
    fmt,
    future::Future,
    path::{Path, PathBuf},
    pin::Pin,
    sync::{Arc, Mutex},
};
use tokio::sync::Notify;

/// One context-facing memory resolver handle. Retrieve/capture semantics are
/// owned by the context work package; this bundle owns only the exact handle
/// and its disposal, so it does not duplicate that contract here.
pub trait MemoryResolver: Send + Sync {}

/// Maintenance handle retained for `ProjectMemoryMaintenanceController`.
pub trait MemoryMaintenanceService: Send + Sync {
    fn run_due_scheduled_maintenance(&self, reason: &str) -> MemoryFuture<'_, ()>;
}

/// The `Pick<EdgeClawMemoryService, "list" | "clear" | "clearSession">`
/// surface consumed by the management methods.
pub trait MemoryManagementService: Send + Sync {
    fn list(&self, limit: usize) -> Result<Vec<JsValue>, MemoryProviderError>;
    fn clear(&self, scope: &str) -> Result<(), MemoryProviderError>;
    fn clear_session(&self, session_key: &str) -> Result<(), MemoryProviderError>;
}

/// The native EdgeClaw service shape, used only by the deprecated
/// `createProvider` injection point. No EdgeClaw implementation lives here.
pub trait NativeMemoryService: Send + Sync {
    fn run_due_scheduled_maintenance(&self, reason: &str) -> MemoryFuture<'_, ()>;
    fn list(&self, limit: usize) -> Result<Vec<JsValue>, MemoryProviderError>;
    fn clear(&self, scope: &str) -> Result<(), MemoryProviderError>;
    fn clear_session(&self, session_key: &str) -> Result<(), MemoryProviderError>;
    fn close(&self);
}

pub type MemoryFuture<'a, T> =
    Pin<Box<dyn Future<Output = Result<T, MemoryProviderError>> + Send + 'a>>;
pub type MemoryDisposeFn = Arc<dyn Fn() -> DisposeFuture<'static> + Send + Sync>;
pub type MemoryProviderFactoryFn = Arc<
    dyn Fn(ProjectMemoryBundleOptions) -> Result<Option<ProjectMemoryProvider>, MemoryBundleError>
        + Send
        + Sync,
>;
pub type NativeProviderFactoryFn = Arc<
    dyn Fn(
            &NativeMemoryProviderOptions,
        ) -> Result<Option<NativeProjectMemoryProvider>, MemoryBundleError>
        + Send
        + Sync,
>;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MemoryProviderError {
    /// JS `error.name`; ordinary injected errors use `"Error"`.
    pub name: String,
    /// JS `error.code`, when the provider supplied one.
    pub code: Option<String>,
    pub message: String,
}
impl MemoryProviderError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            name: "Error".into(),
            code: None,
            message: message.into(),
        }
    }
}

/// The minimum memory configuration that selects the native provider. Parsing
/// stays in the config package; this bundle only sees the parsed values.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MemoryConfig {
    pub enabled: bool,
    pub provider: String,
}

#[derive(Clone)]
pub struct ProjectMemoryBundleOptions {
    pub project_root: PathBuf,
    pub config: Option<MemoryConfig>,
    pub agent_model: Option<String>,
    /// Application-selected provider factory (`providerFactory`).
    pub provider_factory: Option<MemoryProviderFactoryFn>,
    /// Deprecated native injection point (`createProvider`).
    pub create_provider: Option<NativeProviderFactoryFn>,
}
impl ProjectMemoryBundleOptions {
    pub fn new(project_root: impl Into<PathBuf>) -> Self {
        Self {
            project_root: project_root.into(),
            config: None,
            agent_model: None,
            provider_factory: None,
            create_provider: None,
        }
    }
}

/// Input passed to the deprecated `createProvider` hook after stripping
/// `providerFactory` / `createProvider`, matching TS line 57.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NativeMemoryProviderOptions {
    pub project_root: PathBuf,
    pub config: Option<MemoryConfig>,
    pub agent_model: Option<String>,
}

/// Application-selected provider and its exact lifecycle owner.
pub struct ProjectMemoryProvider {
    pub memory: Arc<dyn MemoryResolver>,
    pub maintenance: Option<Arc<dyn MemoryMaintenanceService>>,
    pub management: Option<Arc<dyn MemoryManagementService>>,
    pub dispose: Option<MemoryDisposeFn>,
}

/// The shape returned by the deprecated native injection point.
pub struct NativeProjectMemoryProvider {
    pub provider: Arc<dyn MemoryResolver>,
    pub service: Arc<dyn NativeMemoryService>,
}

/// Resources returned by `stage()`; every field may be absent when the
/// factory reports the provider as disabled.
#[derive(Clone, Default)]
pub struct ProjectMemoryResources {
    pub memory: Option<Arc<dyn MemoryResolver>>,
    pub maintenance: Option<Arc<dyn MemoryMaintenanceService>>,
    pub management: Option<Arc<dyn MemoryManagementService>>,
}
impl ProjectMemoryResources {
    /// TS assigns all three keys whenever a provider was created, even when a
    /// member is `undefined`; an absent provider leaves `{}`.
    pub fn js_keys(&self) -> &'static [&'static str] {
        if self.memory.is_none() && self.maintenance.is_none() && self.management.is_none() {
            &[]
        } else {
            &["memory", "memoryManagement", "memoryService"]
        }
    }
}
impl fmt::Debug for ProjectMemoryResources {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ProjectMemoryResources")
            .field("memory", &self.memory.is_some())
            .field("maintenance", &self.maintenance.is_some())
            .field("management", &self.management.is_some())
            .finish()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MemoryBundleError {
    StageCalledMoreThanOnce,
    AlreadyDisposing,
    Factory(String),
    NativeProviderNotPorted,
}
impl fmt::Display for MemoryBundleError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::StageCalledMoreThanOnce => {
                f.write_str("ProjectMemoryBundle.stage called more than once.")
            }
            Self::AlreadyDisposing => f.write_str("ProjectMemoryBundle is already disposing."),
            Self::Factory(message) => f.write_str(message),
            Self::NativeProviderNotPorted => f.write_str(
                "ProjectMemoryBundle requires an injected provider factory; the native EdgeClaw provider is not ported.",
            ),
        }
    }
}
impl std::error::Error for MemoryBundleError {}
impl From<MemoryBundleError> for RuntimeError {
    fn from(error: MemoryBundleError) -> Self {
        RuntimeError::new(error.to_string())
    }
}

struct BundleState {
    staged: bool,
    dispose_started: bool,
    disposed: bool,
    disposer: Option<MemoryDisposeFn>,
    dispose_error: Option<RuntimeError>,
}
struct BundleShared {
    state: Mutex<BundleState>,
    notify: Notify,
}

/// One project generation's memory provider ownership.
///
/// `stage()` is synchronous and must finish before the bundle is published;
/// `dispose()` is only called on a published (or failed-stage) generation.
/// This mirrors the single-threaded TS contract and keeps staging out of the
/// resource disposal path.
pub struct ProjectMemoryBundle {
    options: ProjectMemoryBundleOptions,
    shared: Arc<BundleShared>,
}
impl ProjectMemoryBundle {
    pub fn new(options: ProjectMemoryBundleOptions) -> Self {
        Self {
            options,
            shared: Arc::new(BundleShared {
                state: Mutex::new(BundleState {
                    staged: false,
                    dispose_started: false,
                    disposed: false,
                    disposer: None,
                    dispose_error: None,
                }),
                notify: Notify::new(),
            }),
        }
    }

    /// `ProjectMemoryBundle.ts:49-80`. `staged` is set before the factory runs,
    /// so a throwing factory still consumes the one allowed stage attempt.
    pub fn stage(&self) -> Result<ProjectMemoryResources, MemoryBundleError> {
        {
            let mut state = self.shared.state.lock().unwrap();
            if state.staged {
                return Err(MemoryBundleError::StageCalledMoreThanOnce);
            }
            if state.dispose_started {
                return Err(MemoryBundleError::AlreadyDisposing);
            }
            state.staged = true;
        }

        let created = if let Some(factory) = &self.options.provider_factory {
            factory(self.options.clone())?
        } else if let Some(factory) = &self.options.create_provider {
            let input = NativeMemoryProviderOptions {
                project_root: self.options.project_root.clone(),
                config: self.options.config.clone(),
                agent_model: self.options.agent_model.clone(),
            };
            factory(&input)?.map(wrap_native)
        } else if self
            .options
            .config
            .as_ref()
            .is_some_and(|config| config.enabled && config.provider == "edgeclaw")
        {
            return Err(MemoryBundleError::NativeProviderNotPorted);
        } else {
            // The native factory returns undefined for missing/disabled
            // configurations; the injected factory covers those states here.
            None
        };

        let (resources, disposer) = match created {
            Some(provider) => (
                ProjectMemoryResources {
                    memory: Some(provider.memory),
                    maintenance: provider.maintenance,
                    management: provider.management,
                },
                provider.dispose,
            ),
            None => (ProjectMemoryResources::default(), None),
        };
        self.shared.state.lock().unwrap().disposer = disposer;
        Ok(resources)
    }

    /// `ProjectMemoryBundle.ts:82-85`. The first call schedules the exact
    /// provider disposer; every call observes the same completion. Disposal is
    /// asynchronous like `Promise.resolve().then(...)`, so work is not run on
    /// the caller's stack. Must be called inside a Tokio runtime.
    pub fn dispose(&self) -> DisposeFuture<'_> {
        let executor = tokio::runtime::Handle::current();
        let first = {
            let mut state = self.shared.state.lock().unwrap();
            if state.dispose_started {
                false
            } else {
                state.dispose_started = true;
                true
            }
        };
        if first {
            let disposer = self.shared.state.lock().unwrap().disposer.take();
            let shared = Arc::clone(&self.shared);
            let task = executor.spawn(async move {
                match disposer {
                    Some(disposer) => disposer().await,
                    None => Ok(()),
                }
            });
            executor.spawn(async move {
                let result = match task.await {
                    Ok(result) => result,
                    Err(error) => Err(RuntimeError::new(format!(
                        "Memory provider dispose task failed: {error}"
                    ))),
                };
                if let Err(error) = result {
                    shared.state.lock().unwrap().dispose_error = Some(error);
                }
                let mut state = shared.state.lock().unwrap();
                state.disposed = true;
                drop(state);
                shared.notify.notify_waiters();
            });
        }
        let shared = Arc::clone(&self.shared);
        Box::pin(async move {
            loop {
                let notified = shared.notify.notified();
                {
                    let state = shared.state.lock().unwrap();
                    if state.disposed {
                        return match &state.dispose_error {
                            Some(error) => Err(error.clone()),
                            None => Ok(()),
                        };
                    }
                }
                notified.await;
            }
        })
    }
}

impl RuntimeResources for ProjectMemoryBundle {
    fn dispose(&self) -> DisposeFuture<'_> {
        ProjectMemoryBundle::dispose(self)
    }
}

fn wrap_native(native: NativeProjectMemoryProvider) -> ProjectMemoryProvider {
    let service = native.service;
    let maintenance: Arc<dyn MemoryMaintenanceService> =
        Arc::new(NativeMaintenance(Arc::clone(&service)));
    let management: Arc<dyn MemoryManagementService> =
        Arc::new(NativeManagement(Arc::clone(&service)));
    let dispose_service = Arc::clone(&service);
    let dispose: MemoryDisposeFn = Arc::new(move || {
        let service = Arc::clone(&dispose_service);
        Box::pin(async move {
            service.close();
            Ok(())
        })
    });
    ProjectMemoryProvider {
        memory: native.provider,
        maintenance: Some(maintenance),
        management: Some(management),
        dispose: Some(dispose),
    }
}

struct NativeMaintenance(Arc<dyn NativeMemoryService>);
impl MemoryMaintenanceService for NativeMaintenance {
    fn run_due_scheduled_maintenance(&self, reason: &str) -> MemoryFuture<'_, ()> {
        self.0.run_due_scheduled_maintenance(reason)
    }
}
struct NativeManagement(Arc<dyn NativeMemoryService>);
impl MemoryManagementService for NativeManagement {
    fn list(&self, limit: usize) -> Result<Vec<JsValue>, MemoryProviderError> {
        self.0.list(limit)
    }
    fn clear(&self, scope: &str) -> Result<(), MemoryProviderError> {
        self.0.clear(scope)
    }
    fn clear_session(&self, session_key: &str) -> Result<(), MemoryProviderError> {
        self.0.clear_session(session_key)
    }
}

/// Project-key resolution plus the per-project `memoryManagement` lookup.
pub trait ProjectMemoryRegistry: Send + Sync {
    /// `dialog.projects.resolveProjectKey`: the input is already the fallback
    /// when the wire field was absent or null.
    fn resolve_project_key(&self, project_key: &Path) -> PathBuf;
    fn memory_management(&self, project_key: &Path) -> Option<Arc<dyn MemoryManagementService>>;
}

#[derive(Clone, Debug, Default)]
pub struct MemoryListInput {
    pub project_key: Option<PathBuf>,
    pub session_key: Option<String>,
}
#[derive(Clone, Debug, Default)]
pub struct MemoryWipeInput {
    pub project_key: Option<PathBuf>,
    pub session_key: Option<String>,
    /// Compared with `=== "session"`; anything else takes the project branch.
    pub scope: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct MemoryListResult {
    pub items: Vec<JsValue>,
}
impl MemoryListResult {
    pub fn to_js(&self) -> JsValue {
        let mut object = JsObject::new();
        object.insert("items".into(), JsValue::Array(self.items.clone()));
        JsValue::Object(object)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MemoryWipeResult {
    pub wiped: bool,
    pub scope: &'static str,
}
impl MemoryWipeResult {
    pub fn to_js(&self) -> JsValue {
        let mut object = JsObject::new();
        object.insert("wiped".into(), JsValue::Bool(self.wiped));
        object.insert("scope".into(), JsValue::from(self.scope));
        JsValue::Object(object)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MemoryProjectionError {
    CapabilityUnavailable,
    InvalidMemoryScope,
    Provider(MemoryProviderError),
}
impl MemoryProjectionError {
    pub fn name(&self) -> &str {
        match self {
            Self::CapabilityUnavailable | Self::InvalidMemoryScope => "DialogGatewayError",
            Self::Provider(error) => &error.name,
        }
    }
    pub fn code(&self) -> Option<&str> {
        match self {
            Self::CapabilityUnavailable => Some("CAPABILITY_UNAVAILABLE"),
            Self::InvalidMemoryScope => Some("INVALID_MEMORY_SCOPE"),
            Self::Provider(error) => error.code.as_deref(),
        }
    }
    pub fn message(&self) -> &str {
        match self {
            Self::CapabilityUnavailable => "Gateway memory provider is unavailable.",
            Self::InvalidMemoryScope => "sessionKey is required for a session-scoped wipe.",
            Self::Provider(error) => &error.message,
        }
    }
    pub fn to_js(&self) -> JsValue {
        let mut object = JsObject::new();
        object.insert("name".into(), JsValue::from(self.name()));
        object.insert(
            "code".into(),
            self.code().map_or(JsValue::Null, JsValue::from),
        );
        object.insert("message".into(), JsValue::from(self.message()));
        JsValue::Object(object)
    }
}

/// The two management methods from `createLocalGateway.ts:1238-1259`.
pub struct MemoryProjection {
    registry: Arc<dyn ProjectMemoryRegistry>,
    fallback_project_root: PathBuf,
}
impl MemoryProjection {
    pub fn new(
        registry: Arc<dyn ProjectMemoryRegistry>,
        fallback_project_root: impl Into<PathBuf>,
    ) -> Self {
        Self {
            registry,
            fallback_project_root: fallback_project_root.into(),
        }
    }

    fn resolution_key(&self, project_key: Option<&Path>) -> PathBuf {
        self.registry
            .resolve_project_key(project_key.unwrap_or(&self.fallback_project_root))
    }

    pub fn list(&self, input: &MemoryListInput) -> Result<MemoryListResult, MemoryProjectionError> {
        let project_key = self.resolution_key(input.project_key.as_deref());
        let management = self
            .registry
            .memory_management(&project_key)
            .ok_or(MemoryProjectionError::CapabilityUnavailable)?;
        let items = management
            .list(100)
            .map_err(MemoryProjectionError::Provider)?;
        let items = match input.session_key.as_deref() {
            // TS truthiness: an empty string lists everything, a whitespace
            // string filters (and never matches the untrimmed comparison).
            Some(session_key) if !session_key.is_empty() => items
                .into_iter()
                .filter(|item| match item.get("sourceSessionKey") {
                    Some(JsValue::String(value)) => value.equals_str(session_key),
                    _ => false,
                })
                .collect(),
            _ => items,
        };
        Ok(MemoryListResult { items })
    }

    pub fn wipe(&self, input: &MemoryWipeInput) -> Result<MemoryWipeResult, MemoryProjectionError> {
        let project_key = self.resolution_key(input.project_key.as_deref());
        let management = self
            .registry
            .memory_management(&project_key)
            .ok_or(MemoryProjectionError::CapabilityUnavailable)?;
        if input.scope.as_deref() == Some("session") {
            let session_key = input
                .session_key
                .as_deref()
                .filter(|session_key| !js_trim(session_key).is_empty())
                .ok_or(MemoryProjectionError::InvalidMemoryScope)?;
            management
                .clear_session(session_key)
                .map_err(MemoryProjectionError::Provider)?;
            return Ok(MemoryWipeResult {
                wiped: true,
                scope: "session",
            });
        }
        management
            .clear("current_project")
            .map_err(MemoryProjectionError::Provider)?;
        Ok(MemoryWipeResult {
            wiped: true,
            scope: "project",
        })
    }
}
