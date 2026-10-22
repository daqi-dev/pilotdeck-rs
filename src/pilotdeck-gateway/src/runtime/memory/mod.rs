//! Project-scoped memory provider composition and management projection.
//!
//! Ports `ProjectMemoryBundle.ts:41-86` (stage/dispose and injected provider
//! ownership) and `createLocalGateway.ts:1238-1259` (`memory_list` /
//! `memory_wipe`). The EdgeClaw sqlite store is introduced by A6.6-pr10a in
//! [`edgeclaw`]; provider composition itself still takes an injected factory.
use crate::runtime::project::{DisposeFuture, RuntimeError, RuntimeResources};

pub mod edgeclaw;
use pilotdeck_protocol::{JsObject, JsString, JsValue};
use std::{
    fmt,
    future::Future,
    path::PathBuf,
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
    /// JS string identity: the raw UTF-16 units, not a lossy `String`, so a
    /// lone surrogate reaches the provider as `"\ud800"` rather than U+FFFD.
    fn clear_session(&self, session_key: &JsString) -> Result<(), MemoryProviderError>;
}

/// The native EdgeClaw service shape, used only by the deprecated
/// `createProvider` injection point. No EdgeClaw implementation lives here.
pub trait NativeMemoryService: Send + Sync {
    fn run_due_scheduled_maintenance(&self, reason: &str) -> MemoryFuture<'_, ()>;
    fn list(&self, limit: usize) -> Result<Vec<JsValue>, MemoryProviderError>;
    fn clear(&self, scope: &str) -> Result<(), MemoryProviderError>;
    /// JS string identity: the raw UTF-16 units, not a lossy `String`, so a
    /// lone surrogate reaches the provider as `"\ud800"` rather than U+FFFD.
    fn clear_session(&self, session_key: &JsString) -> Result<(), MemoryProviderError>;
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
    /// JS `error.message` as UTF-16 units: a `PROJECT_NOT_FOUND` rejection
    /// embeds the raw `projectKey`, including a lone surrogate. Decoding to a
    /// Rust `String` would replacement-decode it and alias another project.
    pub message: JsString,
    /// R19 host boundary: the native port cannot represent this input or
    /// state, so the A8 entry must forward the whole request to the Node host
    /// instead of surfacing this as an ordinary projection error.
    pub requires_host: bool,
}
impl MemoryProviderError {
    pub fn new(message: impl Into<JsString>) -> Self {
        Self {
            name: "Error".into(),
            code: None,
            message: message.into(),
            requires_host: false,
        }
    }
    /// `PROJECT_NOT_FOUND`, `Unknown projectKey: <raw key>`
    /// (projectRegistry.ts:56/62). Build the rejection with the original units
    /// so a lone surrogate survives to the wire instead of becoming U+FFFD.
    pub fn unknown_project_key(project_key: &[u16]) -> Self {
        let mut units = "Unknown projectKey: ".encode_utf16().collect::<Vec<_>>();
        units.extend_from_slice(project_key);
        Self {
            name: "DialogGatewayError".into(),
            code: Some("PROJECT_NOT_FOUND".into()),
            message: JsString::from_utf16(units),
            requires_host: false,
        }
    }
    /// Host boundary for a registry/provider input the native port cannot
    /// represent. Same shape as `runtime/snapshot.rs`: `JS_HOST_REQUIRED`,
    /// empty message, `requires_host`.
    pub fn host_required() -> Self {
        Self {
            name: "Error".into(),
            code: Some("JS_HOST_REQUIRED".into()),
            message: JsString::default(),
            requires_host: true,
        }
    }
    pub fn requires_host(&self) -> bool {
        self.requires_host
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
    /// A throwing `providerFactory` / `createProvider` propagates the exact JS
    /// error (`ProjectMemoryBundle.ts:57-73`): `name`, `code`, and UTF-16
    /// `message` survive instead of collapsing into a plain `String`.
    Factory(MemoryProviderError),
    NativeProviderNotPorted,
}
impl MemoryBundleError {
    /// The original JS error for [`Self::Factory`], for callers that project
    /// the same `{name, code, message}` shape as provider management errors.
    pub fn factory_error(&self) -> Option<&MemoryProviderError> {
        match self {
            Self::Factory(error) => Some(error),
            _ => None,
        }
    }
}
impl fmt::Display for MemoryBundleError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::StageCalledMoreThanOnce => {
                f.write_str("ProjectMemoryBundle.stage called more than once.")
            }
            Self::AlreadyDisposing => f.write_str("ProjectMemoryBundle is already disposing."),
            Self::Factory(error) => {
                f.write_str(&String::from_utf16_lossy(error.message.units()))
            }
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
    /// Set once `stage()` has finished (success, factory error, or panic) and
    /// the disposer slot holds its final value. `dispose()` waits for this
    /// while a stage is in flight, so a disposer published after the dispose
    /// call is still the one scheduled.
    published: bool,
    dispose_started: bool,
    disposed: bool,
    disposer: Option<MemoryDisposeFn>,
    dispose_error: Option<RuntimeError>,
}
struct BundleShared {
    state: Mutex<BundleState>,
    notify: Notify,
}

/// Publishes the stage outcome even when the factory panics, so a concurrent
/// `dispose()` cannot wait forever on a stage that will never finish.
struct StagePublish<'a> {
    shared: &'a BundleShared,
    published: bool,
}
impl StagePublish<'_> {
    fn publish(mut self, disposer: Option<MemoryDisposeFn>) {
        {
            let mut state = self.shared.state.lock().unwrap();
            state.disposer = disposer;
            state.published = true;
        }
        self.published = true;
        self.shared.notify.notify_waiters();
    }
}
impl Drop for StagePublish<'_> {
    fn drop(&mut self) {
        if self.published {
            return;
        }
        {
            let mut state = self.shared.state.lock().unwrap();
            state.published = true;
        }
        self.shared.notify.notify_waiters();
    }
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
                    published: false,
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
    ///
    /// The factory may block; a concurrent `dispose()` must not observe a
    /// taken-but-unset disposer slot. It waits for [`BundleState::published`]
    /// instead, which makes the two calls linearizable: either dispose wins
    /// (and the later stage reports `AlreadyDisposing`), or stage wins and its
    /// exact disposer is scheduled.
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

        // The guard must exist before the factory runs: a panic unwinds past
        // the call, and `Drop` publishes the empty slot so a concurrent
        // `dispose()` cannot wait forever.
        let publish = StagePublish {
            shared: &self.shared,
            published: false,
        };
        let created = if let Some(factory) = &self.options.provider_factory {
            factory(self.options.clone())
        } else if let Some(factory) = &self.options.create_provider {
            let input = NativeMemoryProviderOptions {
                project_root: self.options.project_root.clone(),
                config: self.options.config.clone(),
                agent_model: self.options.agent_model.clone(),
            };
            factory(&input).map(|created| created.map(wrap_native))
        } else if self
            .options
            .config
            .as_ref()
            .is_some_and(|config| config.enabled && config.provider == "edgeclaw")
        {
            Err(MemoryBundleError::NativeProviderNotPorted)
        } else {
            // The native factory returns undefined for missing/disabled
            // configurations; the injected factory covers those states here.
            Ok(None)
        };
        match created {
            Ok(Some(provider)) => {
                let resources = ProjectMemoryResources {
                    memory: Some(provider.memory),
                    maintenance: provider.maintenance,
                    management: provider.management,
                };
                publish.publish(provider.dispose);
                Ok(resources)
            }
            Ok(None) => {
                publish.publish(None);
                Ok(ProjectMemoryResources::default())
            }
            Err(error) => {
                publish.publish(None);
                Err(error)
            }
        }
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
            let wait_shared = Arc::clone(&self.shared);
            let task = executor.spawn(async move {
                let disposer = loop {
                    // Register before the publication check; `enable()` makes
                    // this waiter visible to `notify_waiters()` immediately.
                    let notified = wait_shared.notify.notified();
                    tokio::pin!(notified);
                    notified.as_mut().enable();
                    {
                        let mut state = wait_shared.state.lock().unwrap();
                        // Dispose before stage: `staged` is still false, so TS
                        // resolves immediately and a later stage sees
                        // `AlreadyDisposing`. During a stage the exact
                        // disposer is not known yet, so wait for publication.
                        if !state.staged || state.published {
                            break state.disposer.take();
                        }
                    }
                    notified.await;
                };
                match disposer {
                    Some(disposer) => disposer().await,
                    None => Ok(()),
                }
            });
            let shared = Arc::clone(&self.shared);
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
                // Register before looking at the flag: `enable()` makes this
                // waiter visible to `notify_waiters()` immediately, so a
                // completion between the flag check and the first poll cannot
                // be missed.
                let notified = shared.notify.notified();
                tokio::pin!(notified);
                notified.as_mut().enable();
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
    fn clear_session(&self, session_key: &JsString) -> Result<(), MemoryProviderError> {
        self.0.clear_session(session_key)
    }
}

/// Project-key resolution plus the per-project `memoryManagement` lookup.
///
/// Both seams are fallible because the TS ones can throw: an async
/// `dialog.projects.resolveProjectKey` may reject, and `registry.resolve` runs
/// `assertActive` and the generation factory. Their errors propagate unchanged
/// through the projection; they are never collapsed into `None` and a false
/// `CAPABILITY_UNAVAILABLE`.
pub trait ProjectMemoryRegistry: Send + Sync {
    /// `dialog.projects.resolveProjectKey` compares JS strings by code unit
    /// (projectRegistry.ts:41-59), so the port receives the raw UTF-16 identity
    /// of `input.projectKey` / the fallback root. A lone surrogate must not be
    /// replacement-decoded before the lookup: `"\ud800"` and `"\ufffd"` are
    /// different projects and `from_utf16_lossy` would alias one onto the
    /// other. The resolved key is returned as units for the same reason; the
    /// registry lookup below is keyed by that JS string, never by a `Path`.
    fn resolve_project_key(&self, project_key: &JsString) -> Result<JsString, MemoryProviderError>;
    fn memory_management(
        &self,
        project_key: &JsString,
    ) -> Result<Option<Arc<dyn MemoryManagementService>>, MemoryProviderError>;
}

#[derive(Clone, Debug, Default)]
pub struct MemoryListInput {
    /// Raw wire `projectKey`; absent or null uses the fallback project root.
    pub project_key: Option<JsString>,
    /// Raw wire `sessionKey`, compared with JS truthiness (`===` filtering).
    pub session_key: Option<JsString>,
}
#[derive(Clone, Debug, Default)]
pub struct MemoryWipeInput {
    /// Raw wire `projectKey`; absent or null uses the fallback project root.
    pub project_key: Option<JsString>,
    /// Raw wire `sessionKey`; forwarded to `clearSession` without lossy
    /// replacement-decoding.
    pub session_key: Option<JsString>,
    /// Compared with `=== "session"`; anything else takes the project branch.
    pub scope: Option<JsString>,
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
pub struct MemoryProjectionError {
    code: Option<String>,
    name: String,
    message: JsString,
    /// R19 host boundary; see [`MemoryProviderError::requires_host`].
    requires_host: bool,
}
impl MemoryProjectionError {
    fn gateway(code: &'static str, message: &'static str) -> Self {
        Self {
            code: Some(code.into()),
            name: "DialogGatewayError".into(),
            message: JsString::from(message),
            requires_host: false,
        }
    }
    fn host_required() -> Self {
        MemoryProviderError::host_required().into()
    }
    pub fn name(&self) -> &str {
        &self.name
    }
    pub fn code(&self) -> Option<&str> {
        self.code.as_deref()
    }
    pub fn message(&self) -> &JsString {
        &self.message
    }
    /// True when the native projection hit a host boundary; the A8 entry must
    /// forward the request, not surface this as a TS-shaped error.
    pub fn requires_host(&self) -> bool {
        self.requires_host
    }
    pub fn to_js(&self) -> JsValue {
        let mut object = JsObject::new();
        object.insert("name".into(), JsValue::from(self.name()));
        object.insert(
            "code".into(),
            self.code().map_or(JsValue::Null, JsValue::from),
        );
        object.insert("message".into(), JsValue::String(self.message.clone()));
        JsValue::Object(object)
    }
}
impl From<MemoryProviderError> for MemoryProjectionError {
    fn from(error: MemoryProviderError) -> Self {
        Self {
            code: error.code,
            name: error.name,
            message: error.message,
            requires_host: error.requires_host,
        }
    }
}
impl From<MemoryBundleError> for MemoryProjectionError {
    /// A throwing provider factory propagates the original JS error
    /// (`ProjectMemoryBundle.ts:57-73`), so it projects through the same
    /// `{name, code, message}` shape as a provider management failure instead
    /// of a fixed `"Error"` with the code dropped.
    fn from(error: MemoryBundleError) -> Self {
        match error {
            MemoryBundleError::Factory(error) => error.into(),
            other => MemoryProviderError {
                name: "Error".into(),
                code: None,
                message: JsString::from(other.to_string()),
                requires_host: false,
            }
            .into(),
        }
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

    fn resolution_key(
        &self,
        project_key: Option<&JsString>,
    ) -> Result<JsString, MemoryProjectionError> {
        let fallback;
        let key = match project_key {
            Some(key) => key,
            None => {
                // A host-configured fallback root that is not a JS string
                // cannot be compared by code unit; forward instead of lossy
                // replacement-decoding it into another project's key.
                let root = self
                    .fallback_project_root
                    .to_str()
                    .ok_or_else(MemoryProjectionError::host_required)?;
                fallback = JsString::from(root);
                &fallback
            }
        };
        self.registry
            .resolve_project_key(key)
            .map_err(MemoryProjectionError::from)
    }

    pub fn list(&self, input: &MemoryListInput) -> Result<MemoryListResult, MemoryProjectionError> {
        let project_key = self.resolution_key(input.project_key.as_ref())?;
        let management = self
            .registry
            .memory_management(&project_key)
            .map_err(MemoryProjectionError::from)?
            .ok_or_else(|| {
                MemoryProjectionError::gateway(
                    "CAPABILITY_UNAVAILABLE",
                    "Gateway memory provider is unavailable.",
                )
            })?;
        let items = management.list(100).map_err(MemoryProjectionError::from)?;
        let items = match input.session_key.as_ref() {
            // TS truthiness: an empty string lists everything, a whitespace
            // string filters (and never matches the untrimmed identity).
            Some(session_key) if !session_key.is_empty() => items
                .into_iter()
                .filter(|item| match item.get("sourceSessionKey") {
                    Some(JsValue::String(value)) => value.units() == session_key.units(),
                    _ => false,
                })
                .collect(),
            _ => items,
        };
        Ok(MemoryListResult { items })
    }

    pub fn wipe(&self, input: &MemoryWipeInput) -> Result<MemoryWipeResult, MemoryProjectionError> {
        let project_key = self.resolution_key(input.project_key.as_ref())?;
        let management = self
            .registry
            .memory_management(&project_key)
            .map_err(MemoryProjectionError::from)?
            .ok_or_else(|| {
                MemoryProjectionError::gateway(
                    "CAPABILITY_UNAVAILABLE",
                    "Gateway memory provider is unavailable.",
                )
            })?;
        if input
            .scope
            .as_ref()
            .is_some_and(|scope| scope.equals_str("session"))
        {
            let session_key = input
                .session_key
                .as_ref()
                .filter(|session_key| !js_trim_units(session_key.units()).is_empty())
                .ok_or_else(|| {
                    MemoryProjectionError::gateway(
                        "INVALID_MEMORY_SCOPE",
                        "sessionKey is required for a session-scoped wipe.",
                    )
                })?;
            management
                .clear_session(session_key)
                .map_err(MemoryProjectionError::from)?;
            return Ok(MemoryWipeResult {
                wiped: true,
                scope: "session",
            });
        }
        management
            .clear("current_project")
            .map_err(MemoryProjectionError::from)?;
        Ok(MemoryWipeResult {
            wiped: true,
            scope: "project",
        })
    }
}

/// ECMAScript WhiteSpace + LineTerminator as UTF-16 code units.
pub(crate) fn js_is_space_unit(unit: u16) -> bool {
    matches!(
        unit,
        0x0009..=0x000D
            | 0x0020
            | 0x00A0
            | 0x1680
            | 0x2000..=0x200A
            | 0x2028
            | 0x2029
            | 0x202F
            | 0x205F
            | 0x3000
            | 0xFEFF
    )
}

/// ECMAScript `String.prototype.trim` over raw UTF-16 units, without decoding
/// (a lone surrogate is not whitespace and must keep its identity).
pub(crate) fn js_trim_units(units: &[u16]) -> &[u16] {
    let start = units.iter().position(|&unit| !js_is_space_unit(unit));
    let Some(start) = start else {
        return &[];
    };
    let end = units
        .iter()
        .rposition(|&unit| !js_is_space_unit(unit))
        .unwrap();
    &units[start..=end]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_project_key_keeps_raw_units() {
        let error = MemoryProviderError::unknown_project_key(&[0xD800]);
        assert_eq!(error.name, "DialogGatewayError");
        assert_eq!(error.code.as_deref(), Some("PROJECT_NOT_FOUND"));
        assert!(!error.requires_host());
        assert_eq!(
            error.message.units(),
            "Unknown projectKey: "
                .encode_utf16()
                .chain([0xD800])
                .collect::<Vec<_>>()
                .as_slice()
        );
        assert!(!error.message.units().contains(&0xFFFD));
    }

    #[test]
    fn host_required_is_a_marked_boundary() {
        let error = MemoryProjectionError::host_required();
        assert_eq!(error.name(), "Error");
        assert_eq!(error.code(), Some("JS_HOST_REQUIRED"));
        assert!(error.requires_host());
        assert!(error.message().units().is_empty());
    }
}
