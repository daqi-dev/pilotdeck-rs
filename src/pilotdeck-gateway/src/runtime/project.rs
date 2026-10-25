//! Generation/lease ownership from ProjectRuntimeRegistry.ts:393-523,1540-1693.
//!
//! Staging is synchronous, as in TS. The registry mutex is the publication
//! boundary; factories must not reenter this registry. No resource disposal or
//! lease drain is awaited under that mutex. Cleanup tasks outlive their callers,
//! so cancelling reload/dispose/release cannot strand a retired generation.
use pilotdeck_protocol::JsString;
use std::{
    any::Any,
    fmt,
    future::Future,
    path::{Path, PathBuf},
    pin::Pin,
    sync::{Arc, Mutex, Weak},
};
use tokio::{runtime::Handle, sync::watch};

pub type RuntimeResult = Result<(), RuntimeError>;
pub type DisposeFuture<'a> = Pin<Box<dyn Future<Output = RuntimeResult> + Send + 'a>>;

/// Opaque pass-through for the TS `TelemetryClient`.
///
/// The native runtime has no telemetry port yet, so no method is called and no
/// event shape is invented. The exact host object is retained (and can be
/// downcast by the future provider port) instead of being dropped at the
/// staging boundary.
pub type TelemetryClient = Arc<dyn Any + Send + Sync>;

/// The exact JS error shape when a failure crossed a JS boundary.
///
/// `message` stays as raw UTF-16 units because a provider rejection may embed a
/// lone surrogate; decoding to `String` would replacement-decode it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RuntimeJsError {
    pub name: String,
    pub code: Option<String>,
    pub message: JsString,
    /// R19 host boundary: the native port cannot represent the input/state, so
    /// the A8 entry must forward the whole request instead of surfacing this as
    /// an ordinary projection error.
    pub requires_host: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RuntimeError {
    pub message: String,
    pub causes: Vec<RuntimeError>,
    /// Present when this failure originated as a JS error at a staging
    /// boundary. `message` remains the lossy display form; this keeps the
    /// original `{name, code, message, requires_host}` intact for consumers.
    /// Boxed so `RuntimeError` stays small enough for the many existing
    /// `Result<_, RuntimeError>` signatures (clippy `result_large_err`).
    pub js_error: Option<Box<RuntimeJsError>>,
}
impl RuntimeError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            causes: Vec::new(),
            js_error: None,
        }
    }
    /// Retain the original JS error alongside the display message.
    pub fn with_js_error(mut self, js_error: RuntimeJsError) -> Self {
        self.js_error = Some(Box::new(js_error));
        self
    }
    pub fn js_error(&self) -> Option<&RuntimeJsError> {
        self.js_error.as_deref()
    }
    fn aggregate(message: &str, mut failures: Vec<Self>) -> RuntimeResult {
        match failures.len() {
            0 => Ok(()),
            1 => Err(failures.remove(0)),
            _ => Err(Self {
                message: message.into(),
                causes: failures,
                js_error: None,
            }),
        }
    }
}
impl fmt::Display for RuntimeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}
impl std::error::Error for RuntimeError {}

/// One package's resources (model, tools, context, ...). Register partially
/// initialized groups before a fallible stage step so rollback owns them too.
/// Shutdown must be synchronous; async work belongs in dispose. A group's
/// dispose must clean up all its members even when one member fails.
pub trait RuntimeResources: Any + Send + Sync {
    fn shutdown(&self) {}
    fn dispose(&self) -> DisposeFuture<'_>;
}

#[derive(Default)]
pub struct ResourceBundle {
    groups: Vec<Arc<dyn RuntimeResources>>,
}
impl ResourceBundle {
    pub fn add<R: RuntimeResources>(&mut self, resources: R) {
        self.groups.push(Arc::new(resources));
    }
    pub fn get<R: RuntimeResources>(&self) -> Option<&R> {
        self.groups
            .iter()
            .find_map(|r| (r.as_ref() as &dyn Any).downcast_ref())
    }
    async fn dispose(&self, executor: &Handle, staged: bool) -> RuntimeResult {
        let mut failures = Vec::new();
        // Registration order is disposal order. Each package can independently
        // define its internal order without editing this lifecycle module.
        for group in &self.groups {
            let group = Arc::clone(group);
            // A panic in one package must not skip the remaining packages or
            // leave completion waiters hanging forever.
            let result = executor
                .spawn(async move {
                    if staged {
                        group.shutdown();
                    }
                    group.dispose().await
                })
                .await;
            match result {
                Ok(Ok(())) => {}
                Ok(Err(error)) => failures.push(error),
                Err(error) => failures.push(RuntimeError::new(format!(
                    "Runtime resource cleanup task failed: {error}"
                ))),
            }
        }
        RuntimeError::aggregate("Failed to dispose project runtime resources.", failures)
    }
}

pub trait ProjectRuntimeFactory: Send + Sync {
    /// Add each acquired group to `resources` before any later step can fail.
    /// No I/O is performed by the registry itself. This synchronous callback
    /// follows the TS buildRuntime contract and must not reenter the registry.
    fn stage(&self, project_root: &Path, resources: &mut ResourceBundle) -> RuntimeResult;
}

pub mod context_storage;
pub mod custom_routers;
/// The production factory (`buildRuntime`): config read and resource-family
/// staging with the baseline's rollback order.
mod factory;
pub mod interaction;
pub mod interaction_events;
pub mod interaction_hooks;
pub mod interaction_permission;
pub mod interaction_steer;
pub mod memory_bundle;
pub mod model;
pub mod persistence;
pub mod plugins;
pub mod tools_mcp;
pub use factory::{
    ConfigSource, Families, Family, FamilyResource, InteractionProfile, InteractionProfileName,
    NativeConfigSource, Now, ProjectResources, ProjectResourcesFactory, ProjectResourcesOptions,
    Resource, ResourceFamilySource, RuntimeContextSurface, RuntimeProfile, RuntimeProfileOverrides,
    SandboxMode, StageContext, StageSink, UnsupportedFamily,
};
pub use memory_bundle::MemoryBundleFamilySource;
impl<F> ProjectRuntimeFactory for F
where
    F: Fn(&Path, &mut ResourceBundle) -> RuntimeResult + Send + Sync,
{
    fn stage(&self, root: &Path, resources: &mut ResourceBundle) -> RuntimeResult {
        self(root, resources)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RuntimeState {
    Active,
    Retired,
    Disposed,
}
impl RuntimeState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Retired => "retired",
            Self::Disposed => "disposed",
        }
    }
}
struct GenerationState {
    phase: RuntimeState,
    leases: usize,
    cleanup_started: bool,
    cleanup_enabled: bool,
}
type Completion = watch::Receiver<Option<RuntimeResult>>;

pub struct ProjectRuntime {
    project_root: PathBuf,
    resources: ResourceBundle,
    staged: bool,
    state: Mutex<GenerationState>,
    executor: Handle,
    completion: watch::Sender<Option<RuntimeResult>>,
}
impl ProjectRuntime {
    fn new(
        project_root: PathBuf,
        resources: ResourceBundle,
        executor: Handle,
        staged: bool,
    ) -> Arc<Self> {
        Arc::new(Self {
            project_root,
            resources,
            executor,
            staged,
            state: Mutex::new(GenerationState {
                phase: RuntimeState::Active,
                leases: 0,
                cleanup_started: false,
                cleanup_enabled: true,
            }),
            completion: watch::channel(None).0,
        })
    }
    pub fn project_root(&self) -> &Path {
        &self.project_root
    }
    /// Access a group's operations through a live lease while using resources.
    pub fn resources(&self) -> &ResourceBundle {
        &self.resources
    }
    pub fn state(&self) -> RuntimeState {
        self.state.lock().unwrap().phase
    }
    pub fn lease_count(&self) -> usize {
        self.state.lock().unwrap().leases
    }
    pub fn acquire(self: &Arc<Self>) -> Result<RuntimeLease, RuntimeError> {
        let mut state = self.state.lock().unwrap();
        if state.phase != RuntimeState::Active {
            return Err(RuntimeError::new(format!(
                "Project runtime is {}.",
                state.phase.as_str()
            )));
        }
        state.leases += 1;
        Ok(RuntimeLease {
            runtime: Arc::clone(self),
            released: false,
        })
    }
    fn retire(self: &Arc<Self>) {
        let mut state = self.state.lock().unwrap();
        if state.phase == RuntimeState::Active {
            state.phase = RuntimeState::Retired;
        }
        self.start_cleanup(&mut state);
    }
    fn defer_retirement(&self) {
        let mut state = self.state.lock().unwrap();
        if state.phase == RuntimeState::Active {
            state.phase = RuntimeState::Retired;
            state.cleanup_enabled = false;
        }
    }
    fn enable_cleanup(self: &Arc<Self>) -> bool {
        let mut state = self.state.lock().unwrap();
        state.cleanup_enabled = true;
        self.start_cleanup(&mut state);
        state.leases == 0
    }
    fn release(self: &Arc<Self>) -> bool {
        let mut state = self.state.lock().unwrap();
        debug_assert!(state.leases > 0);
        state.leases -= 1;
        self.start_cleanup(&mut state);
        state.phase != RuntimeState::Active && state.leases == 0
    }
    fn start_cleanup(self: &Arc<Self>, state: &mut GenerationState) {
        if state.phase != RuntimeState::Retired
            || state.leases != 0
            || state.cleanup_started
            || !state.cleanup_enabled
        {
            return;
        }
        state.cleanup_started = true;
        let runtime = Arc::clone(self);
        super::cleanup_task::spawn_with_error(
            &self.executor,
            self.completion.clone(),
            RuntimeError::new(
                "Project runtime cleanup task was dropped or panicked before completion.",
            ),
            async move {
                let result = runtime
                    .resources
                    .dispose(&runtime.executor, runtime.staged)
                    .await;
                runtime.state.lock().unwrap().phase = RuntimeState::Disposed;
                result
            },
        );
    }
    /// Wait for retirement, outstanding leases, and asynchronous cleanup.
    /// Calling this on an active generation waits until it is retired.
    pub async fn wait_disposed(&self) -> RuntimeResult {
        wait_completion(self.completion.subscribe()).await
    }
}

/// Not Clone: exactly one lease count is owned by each guard. Drop releases it
/// even when a session is cancelled. Explicit release can await final cleanup.
pub struct RuntimeLease {
    runtime: Arc<ProjectRuntime>,
    released: bool,
}
impl RuntimeLease {
    pub fn runtime(&self) -> &Arc<ProjectRuntime> {
        &self.runtime
    }
    pub async fn release(&mut self) -> RuntimeResult {
        if self.released {
            return Ok(());
        }
        self.released = true;
        if self.runtime.release() {
            self.runtime.wait_disposed().await
        } else {
            Ok(())
        }
    }
}
impl Drop for RuntimeLease {
    fn drop(&mut self) {
        if !self.released {
            self.released = true;
            self.runtime.release();
        }
    }
}
async fn wait_completion(mut completion: Completion) -> RuntimeResult {
    loop {
        if let Some(result) = completion.borrow().clone() {
            return result;
        }
        if completion.changed().await.is_err() {
            return Err(RuntimeError::new(
                "Runtime cleanup stopped before completion.",
            ));
        }
    }
}

struct RegistryState {
    // Vec preserves JS Map insertion order for staging and cleanup.
    current: Vec<(PathBuf, Arc<ProjectRuntime>)>,
    // Weak tracking includes retired/incomplete generations without retaining
    // every completed generation forever. Live leases and cleanup own them.
    generations: Vec<Weak<ProjectRuntime>>,
    disposed: bool,
    disposal: Option<Completion>,
}

/// Construct within a running Tokio runtime; keep that runtime alive until
/// `dispose` completes. Explicit dispose is required before host shutdown.
pub struct ProjectRuntimeRegistry {
    fallback_project_root: PathBuf,
    factory: Arc<dyn ProjectRuntimeFactory>,
    executor: Handle,
    state: Mutex<RegistryState>,
    reload_gate: Arc<tokio::sync::Mutex<()>>,
}
impl ProjectRuntimeRegistry {
    pub fn new(
        fallback_project_root: impl Into<PathBuf>,
        factory: Arc<dyn ProjectRuntimeFactory>,
    ) -> Self {
        Self {
            fallback_project_root: fallback_project_root.into(),
            factory,
            executor: Handle::current(),
            state: Mutex::new(RegistryState {
                current: Vec::new(),
                generations: Vec::new(),
                disposed: false,
                disposal: None,
            }),
            reload_gate: Arc::new(tokio::sync::Mutex::new(())),
        }
    }
    /// Whether `dispose` has been called (reload/resolve now fail).
    pub fn is_disposed(&self) -> bool {
        self.state.lock().unwrap().disposed
    }
    fn assert_active(state: &RegistryState) -> RuntimeResult {
        if state.disposed {
            Err(RuntimeError::new("Project runtime registry is disposed."))
        } else {
            Ok(())
        }
    }
    fn stage(
        &self,
        root: &Path,
        state: &mut RegistryState,
    ) -> (Arc<ProjectRuntime>, RuntimeResult) {
        let mut resources = ResourceBundle::default();
        // A factory panic is this registry's analogue of a TS buildRuntime
        // throw: it must take the ordinary rollback path for every group it
        // already registered, and must not poison the registry mutex held by
        // the caller (#301). The bundle only ever gains whole groups, so it is
        // consistent at any unwind point.
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            self.factory.stage(root, &mut resources)
        }))
        .unwrap_or_else(|payload| {
            let detail = payload
                .downcast_ref::<&str>()
                .map(|s| (*s).to_owned())
                .or_else(|| payload.downcast_ref::<String>().cloned())
                .unwrap_or_else(|| "non-string panic payload".into());
            Err(RuntimeError::new(format!(
                "Project runtime factory panicked: {detail}"
            )))
        });
        let runtime = ProjectRuntime::new(
            root.to_owned(),
            resources,
            self.executor.clone(),
            result.is_ok(),
        );
        state
            .generations
            .retain(|generation| generation.strong_count() > 0);
        state.generations.push(Arc::downgrade(&runtime));
        (runtime, result)
    }
    fn resolve_locked(
        &self,
        key: Option<&Path>,
        state: &mut RegistryState,
    ) -> Result<Arc<ProjectRuntime>, RuntimeError> {
        Self::assert_active(state)?;
        let root = resolve_path(key.unwrap_or(&self.fallback_project_root))?;
        if let Some((_, runtime)) = state.current.iter().find(|(path, _)| path == &root) {
            return Ok(Arc::clone(runtime));
        }
        let (runtime, result) = self.stage(&root, state);
        if let Err(error) = result {
            runtime.retire();
            return Err(error);
        }
        state.current.push((root, Arc::clone(&runtime)));
        Ok(runtime)
    }
    pub fn resolve(&self, project_key: Option<&Path>) -> Result<Arc<ProjectRuntime>, RuntimeError> {
        self.resolve_locked(project_key, &mut self.state.lock().unwrap())
    }
    /// Resolve and acquire at one publication boundary. Use this for session
    /// admission; separate resolve/acquire may legitimately race retirement.
    pub fn acquire(&self, project_key: Option<&Path>) -> Result<RuntimeLease, RuntimeError> {
        let mut state = self.state.lock().unwrap();
        self.resolve_locked(project_key, &mut state)?.acquire()
    }
    /// TS invalidate uses an exact cache key, unlike resolve. None or an empty
    /// path invalidates all projects; a relative key is not normalized here.
    pub fn invalidate(&self, project_root: Option<&Path>) {
        let mut state = self.state.lock().unwrap();
        if state.disposed {
            return;
        }
        state.current.retain(|(root, runtime)| {
            let remove = project_root.is_none_or(|key| {
                key.as_os_str().is_empty() || key.as_os_str() == root.as_os_str()
            });
            if remove {
                runtime.retire();
            }
            !remove
        });
    }
    /// Stage every current project before publishing any replacement. Failed
    /// candidates (including a partial build) are all cleaned up, and cleanup
    /// errors are returned alongside the original staging error.
    pub async fn reload(&self) -> RuntimeResult {
        Self::assert_active(&self.state.lock().unwrap())?;
        let serial = Arc::clone(&self.reload_gate).lock_owned().await;
        let failed = {
            let mut state = self.state.lock().unwrap();
            Self::assert_active(&state)?;
            let roots: Vec<_> = state.current.iter().map(|(root, _)| root.clone()).collect();
            let mut staged = Vec::new();
            let mut failure = None;
            for root in roots {
                let (runtime, result) = self.stage(&root, &mut state);
                staged.push((root, runtime));
                if let Err(error) = result {
                    failure = Some(error);
                    break;
                }
            }
            if let Some(error) = failure {
                // Schedule cleanup before the first await (cancellation safe).
                if let Some((_, partial)) = staged.last() {
                    partial.retire();
                }
                for (_, runtime) in &staged[..staged.len() - 1] {
                    runtime.retire();
                }
                Some((error, staged))
            } else {
                let old = std::mem::replace(&mut state.current, staged);
                for (_, runtime) in old {
                    runtime.retire();
                }
                None
            }
        };
        if let Some((error, staged)) = failed {
            let (sender, completion) = watch::channel(None);
            super::cleanup_task::spawn_with_error(
                &self.executor,
                sender,
                RuntimeError::new(
                    "Project runtime reload rollback task was dropped or panicked before completion.",
                ),
                async move {
                    // Keep the queue blocked through rollback even if the caller
                    // cancels its reload future while waiting for cleanup.
                    let _serial = serial;
                    let mut failures = vec![error];
                    let mut staged = staged;
                    staged.rotate_right(1); // TS: partial cleanup, then staged map order.
                    for (_, runtime) in staged {
                        if let Err(error) = runtime.wait_disposed().await {
                            failures.push(error);
                        }
                    }
                    RuntimeError::aggregate(
                        "Failed to stage and clean up project runtime reload.",
                        failures,
                    )
                },
            );
            wait_completion(completion).await
        } else {
            Ok(())
        }
    }
    /// Close admission immediately on first poll, then drain *all* generations,
    /// including ones removed by invalidate/reload. Unlike TS's leased-runtime
    /// skip, the task's explicit shutdown contract requires awaiting leases.
    /// Ledger: specs/gateway-parity/accepted-diffs.md A7-PROJECT-DISPOSE-001 (unapproved).
    /// Repeated/concurrent calls observe one result; cancellation is harmless.
    pub async fn dispose(&self) -> RuntimeResult {
        let completion = {
            let mut state = self.state.lock().unwrap();
            if let Some(completion) = &state.disposal {
                completion.clone()
            } else {
                state.disposed = true;
                let mut runtimes: Vec<_> = state
                    .current
                    .iter()
                    .map(|(_, runtime)| Arc::clone(runtime))
                    .collect();
                for runtime in state.generations.iter().filter_map(Weak::upgrade) {
                    if !runtimes
                        .iter()
                        .any(|existing| Arc::ptr_eq(existing, &runtime))
                    {
                        runtimes.push(runtime);
                    }
                }
                state.current.clear();
                // Close stale-handle admission now, but preserve TS's sequential
                // cleanup order for current, unleased projects.
                for runtime in &runtimes {
                    runtime.defer_retirement();
                }
                let (sender, completion) = watch::channel(None);
                state.disposal = Some(completion.clone());
                let reload_gate = Arc::clone(&self.reload_gate);
                super::cleanup_task::spawn_with_error(
                    &self.executor,
                    sender,
                    RuntimeError::new(
                        "Project runtime dispose task was dropped or panicked before completion.",
                    ),
                    async move {
                        // TS dispose chains after reloadTail, including rollback.
                        let reload = reload_gate.lock().await;
                        drop(reload); // Admission is closed; never hold this while draining leases.
                        let mut failures = Vec::new();
                        let mut leased = Vec::new();
                        for runtime in runtimes {
                            // Completed old/failed candidates no longer belong to
                            // TS's pending-disposal set; do not replay their errors.
                            if runtime.state() == RuntimeState::Disposed {
                                continue;
                            }
                            if runtime.enable_cleanup() {
                                if let Err(error) = runtime.wait_disposed().await {
                                    failures.push(error);
                                }
                            } else {
                                leased.push(runtime);
                            }
                        }
                        for runtime in leased {
                            if let Err(error) = runtime.wait_disposed().await {
                                failures.push(error);
                            }
                        }
                        RuntimeError::aggregate(
                            "Failed to dispose project runtime generations.",
                            failures,
                        )
                    },
                );
                completion
            }
        };
        wait_completion(completion).await
    }
}

// Node path.resolve is lexical and does not require the project to exist.
// The normalization itself is shared with the config/session path seams.
fn resolve_path(path: &Path) -> Result<PathBuf, RuntimeError> {
    resolve_path_with_cwd(path, std::env::current_dir)
}

/// Node `path.resolve` only consults `process.cwd()` for a relative input
/// (libuv's posix resolve reaches `cwd()` only when no absolute segment was
/// seen). A deleted working directory must therefore still resolve absolute
/// project keys. The getter is injected so the test can prove it is untouched
/// without mutating the process-wide cwd.
fn resolve_path_with_cwd(
    path: &Path,
    cwd: impl FnOnce() -> std::io::Result<PathBuf>,
) -> Result<PathBuf, RuntimeError> {
    if path.is_absolute() {
        return Ok(crate::metadata::resolve_path(Path::new("/"), path));
    }
    let cwd = cwd().map_err(|error| RuntimeError::new(error.to_string()))?;
    Ok(crate::metadata::resolve_path(&cwd, path))
}

#[cfg(test)]
#[path = "project_tests.rs"]
mod tests;
