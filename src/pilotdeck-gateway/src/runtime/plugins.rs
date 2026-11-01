//! PluginRuntime generation / lease model and GatewayCommandCatalogBundle,
//! ported from the fixed TypeScript baseline:
//!
//! - `src/extension/plugins/runtime/PluginRuntime.ts:245-491` (generation,
//!   refresh, createView, acquireSessionContributions, acquireCommandCatalog,
//!   acquireContributionSnapshot, dispose)
//! - `src/cli/GatewayCommandCatalogBundle.ts:46-70` (commandsList: refresh →
//!   acquire → list → finally release; release failure only warns)
//!
//! Disk discovery/loading is staged by `loader::load_from_disk`. The runtime
//! owns generation publication, failure rollback, and lease retirement.
//!
//! ## JsValue / manifest traversal
//!
//! All traversal over JsValue and plugin manifest fields uses explicit iteration
//! (no recursion). Use `crate::js_value` bounded helpers when computing depths.
//!
//! ## Concurrency contract
//!
//! - `refresh` is single-flight: concurrent callers share one result.
//! - `dispose` closes admission immediately; it waits for the pending refresh
//!   and all outstanding leases before completing.
//! - Lease release waits for the disposal of every retired plugin held by that
//!   lease. A release error reaches only the releaser, not the active runtime.
//! - Disposal batches reserve each instance once under the state lock. Callback
//!   construction and all polls run outside that lock, with panics converted to
//!   disposal errors. Start barriers preserve batch order; callbacks resume
//!   independently after every synchronous prologue in their batch has run.
//! - Path and projectKey identity are preserved as UTF-8 (lone surrogates
//!   round-trip via replacement character; that matches `String::from_utf16_lossy`
//!   and is the same as Node's filesystem access from a JsString).

use pilotdeck_protocol::JsObject;
type ResolveProjectKeyFn = std::sync::Arc<
    dyn Fn(
            String,
        ) -> std::pin::Pin<
            Box<
                dyn std::future::Future<Output = Result<String, PluginRuntimeError>>
                    + Send
                    + 'static,
            >,
        > + Send
        + Sync,
>;
use std::{collections::HashMap, future::Future, pin::Pin, sync::Arc};
use tokio::sync::{Mutex as TokioMutex, Notify, watch};

// ─────────────────────────────────────────────────────────── public re-exports

pub use contributions::{
    PluginCommandCatalogLease, PluginCommandCatalogSnapshot, PluginCommandContribution,
    PluginContributionLease, PluginContributionSnapshot, PluginMcpInstruction,
    PluginMcpInstructionEntry, PluginOutputStyleContribution, PluginRouterContribution,
    PluginSessionContributionLease, PluginSessionContributionSnapshot, PluginSkillContribution,
    PluginToolContribution,
};
pub use loader::{LoadedPlugin, PluginRefreshFailure, PluginRefreshResult, PluginSource};
pub use options::{PilotExtensionPaths, PluginRuntimeOptions};

// ─────────────────────────────────────────────────────────── sub-modules

pub mod catalog;
mod contributions;
pub mod loader;
mod options;

// ─────────────────────────────────────────────────────────── error

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PluginRuntimeError {
    pub message: String,
    pub code: Option<&'static str>,
    pub causes: Vec<PluginRuntimeError>,
}

impl PluginRuntimeError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            code: None,
            causes: Vec::new(),
        }
    }
    pub fn gateway(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code: Some(code),
            ..Self::new(message)
        }
    }
    pub fn aggregate(
        message: &str,
        errors: Vec<PluginRuntimeError>,
    ) -> Result<(), PluginRuntimeError> {
        match errors.len() {
            0 => Ok(()),
            1 => Err(errors.into_iter().next().unwrap()),
            _ => Err(PluginRuntimeError {
                message: message.into(),
                code: None,
                causes: errors,
            }),
        }
    }
    /// AggregateError from JS: .errors[] at depth 1.
    pub fn aggregate_js(message: &str, causes: Vec<PluginRuntimeError>) -> PluginRuntimeError {
        PluginRuntimeError {
            message: message.into(),
            code: None,
            causes,
        }
    }
    pub fn is_aggregate(&self) -> bool {
        !self.causes.is_empty()
    }
}

impl std::fmt::Display for PluginRuntimeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for PluginRuntimeError {}

// ─────────────────────────────────────────────────────────── PluginRegistry

/// Mirrors `PluginRegistry.ts`. Insertion order is JS Map order.
pub(super) struct PluginRegistry {
    /// Active plugins keyed by `name@source`.
    plugins: HashMap<String, Arc<LoadedPlugin>>,
    /// Stable insertion order for list() (JS Map preserves it).
    order: Vec<String>,
    retired_plugins: Vec<Arc<LoadedPlugin>>,
    // JS Map order also determines the outer AggregateError cause order.
    pending_disposals: Vec<PendingDisposal>,
    disposal_waiters: Vec<Arc<LoadedPlugin>>,
    // Batches reserve instances under the state lock, but execute user code
    // outside it. A start barrier preserves order across concurrent events.
    disposal_start_receiver: Option<watch::Receiver<Option<Result<(), PluginRuntimeError>>>>,
    #[cfg(test)]
    after_disposal_first_poll: Option<Box<dyn FnOnce(bool) + Send>>,
    lease_counts: HashMap<usize, usize>,
    lease_failure: watch::Sender<Option<Result<(), PluginRuntimeError>>>,
    waiters: Arc<Notify>,
    state: RegistryState,
    generation: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RegistryState {
    Active,
    Draining,
    Disposed,
}

impl RegistryState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Draining => "draining",
            Self::Disposed => "disposed",
        }
    }
}

struct RegistryLease {
    generation: u64,
    plugins: Vec<Arc<LoadedPlugin>>,
}

impl PluginRegistry {
    fn new() -> Self {
        Self {
            plugins: HashMap::new(),
            order: Vec::new(),
            retired_plugins: Vec::new(),
            pending_disposals: Vec::new(),
            disposal_waiters: Vec::new(),
            disposal_start_receiver: None,
            #[cfg(test)]
            after_disposal_first_poll: None,
            lease_counts: HashMap::new(),
            lease_failure: watch::channel(None).0,
            waiters: Arc::new(Notify::new()),
            state: RegistryState::Active,
            generation: 0,
        }
    }

    fn list(&self) -> Vec<Arc<LoadedPlugin>> {
        self.order
            .iter()
            .filter_map(|k| self.plugins.get(k).cloned())
            .collect()
    }

    fn current_generation(&self) -> u64 {
        self.generation
    }

    fn assert_active(&self, action: &str) -> Result<(), PluginRuntimeError> {
        if let Some(Err(error)) = &*self.lease_failure.borrow() {
            return Err(error.clone());
        }
        if self.state != RegistryState::Active {
            Err(PluginRuntimeError::new(format!(
                "Cannot {action} plugins; plugin registry is {}.",
                self.state.as_str()
            )))
        } else {
            Ok(())
        }
    }

    fn acquire(&mut self) -> Result<RegistryLease, PluginRuntimeError> {
        self.assert_active("acquire")?;
        let plugins = self.list();
        for plugin in &plugins {
            let count = self
                .lease_counts
                .entry(Arc::as_ptr(plugin) as usize)
                .or_insert(0);
            *count += 1;
        }
        Ok(RegistryLease {
            generation: self.generation,
            plugins,
        })
    }

    fn replace_all(
        &mut self,
        plugins: Vec<Arc<LoadedPlugin>>,
    ) -> Result<ReplaceResult, PluginRuntimeError> {
        self.assert_active("replace")?;
        // same key logic as TS: `${name}@${source}`
        let previous = self.list();
        let next = plugins.clone();
        let next_keys: std::collections::HashSet<String> = next
            .iter()
            .map(|p| format!("{}@{}", p.name, p.source.as_str()))
            .collect();
        let removed: Vec<Arc<LoadedPlugin>> = previous
            .iter()
            .filter(|p| !next_keys.contains(&format!("{}@{}", p.name, p.source.as_str())))
            .cloned()
            .collect();
        // retired = previous instances no longer in next (by pointer, not key)
        let retired: Vec<Arc<LoadedPlugin>> = previous
            .iter()
            .filter(|p| !next.iter().any(|n| Arc::ptr_eq(n, p)))
            .cloned()
            .collect();
        let retiring_lease_count: usize = retired
            .iter()
            .map(|p| {
                self.lease_counts
                    .get(&(Arc::as_ptr(p) as usize))
                    .copied()
                    .unwrap_or(0)
            })
            .sum();

        self.plugins.clear();
        self.order.clear();
        for plugin in &next {
            let key = format!("{}@{}", plugin.name, plugin.source.as_str());
            if self
                .plugins
                .insert(key.clone(), Arc::clone(plugin))
                .is_none()
            {
                self.order.push(key);
            }
        }
        self.waiters.notify_waiters();
        for p in &retired {
            // TS retiredPlugins is a Set of instances, including reactivated
            // builtins that retire again while their original lease is held.
            if !self.retired_plugins.iter().any(|old| Arc::ptr_eq(old, p)) {
                self.retired_plugins.push(Arc::clone(p));
            }
        }
        self.generation += 1;

        Ok(ReplaceResult {
            generation: self.generation,
            previous,
            next,
            removed,
            retired,
            retiring_lease_count,
        })
    }

    fn is_plugin_referenced(&self, plugin: &Arc<LoadedPlugin>) -> bool {
        let key = format!("{}@{}", plugin.name, plugin.source.as_str());
        if self
            .plugins
            .get(&key)
            .is_some_and(|active| Arc::ptr_eq(active, plugin))
        {
            return true;
        }
        self.lease_counts
            .get(&(Arc::as_ptr(plugin) as usize))
            .copied()
            .unwrap_or(0)
            > 0
    }
}

struct ReplaceResult {
    generation: u64,
    previous: Vec<Arc<LoadedPlugin>>,
    next: Vec<Arc<LoadedPlugin>>,
    removed: Vec<Arc<LoadedPlugin>>,
    retired: Vec<Arc<LoadedPlugin>>,
    retiring_lease_count: usize,
}

// ─────────────────────────────────────────────────────────── PluginRuntimeState

pub(super) struct PluginRuntimeState {
    pub(super) registry: PluginRegistry,
    refresh_sender: Option<watch::Sender<Option<Result<PluginRefreshResult, PluginRuntimeError>>>>,
    refresh_receiver:
        Option<watch::Receiver<Option<Result<PluginRefreshResult, PluginRuntimeError>>>>,
    disposal_requested: bool,
    dispose_sender: Option<watch::Sender<Option<Result<(), PluginRuntimeError>>>>,
    dispose_receiver: Option<watch::Receiver<Option<Result<(), PluginRuntimeError>>>>,
}

// ─────────────────────────────────────────────────────────── PluginRuntime

/// `PluginRuntime` — thread-safe owner of the current plugin generation.
pub struct PluginRuntime {
    options: Arc<PluginRuntimeOptions>,
    state: Arc<TokioMutex<PluginRuntimeState>>,
    lease_failure: watch::Sender<Option<Result<(), PluginRuntimeError>>>,
}

impl PluginRuntime {
    pub fn new(options: PluginRuntimeOptions) -> Self {
        let registry = PluginRegistry::new();
        let lease_failure = registry.lease_failure.clone();
        Self {
            options: Arc::new(options),
            state: Arc::new(TokioMutex::new(PluginRuntimeState {
                registry,
                refresh_sender: None,
                refresh_receiver: None,
                disposal_requested: false,
                dispose_sender: None,
                dispose_receiver: None,
            })),
            lease_failure,
        }
    }

    pub async fn refresh(&self) -> Result<Vec<Arc<LoadedPlugin>>, PluginRuntimeError> {
        Ok(self.refresh_with_report().await?.next)
    }

    /// Single-flight: concurrent callers share one pending refresh result.
    pub async fn refresh_with_report(&self) -> Result<PluginRefreshResult, PluginRuntimeError> {
        if let Some(Err(error)) = &*self.lease_failure.borrow() {
            return Err(error.clone());
        }
        let rx = {
            let mut st = self.state.lock().await;
            // A release can be lost while this caller is queued on state.
            if let Some(Err(error)) = &*st.registry.lease_failure.borrow() {
                return Err(error.clone());
            }
            if st.disposal_requested || st.registry.state != RegistryState::Active {
                return Err(PluginRuntimeError::new(format!(
                    "Cannot refresh plugins; plugin runtime is {}.",
                    if st.disposal_requested {
                        "draining"
                    } else {
                        st.registry.state.as_str()
                    }
                )));
            }
            if let Some(rx) = &st.refresh_receiver {
                rx.clone()
            } else {
                let (tx, rx) = watch::channel(None);
                st.refresh_sender = Some(tx.clone());
                st.refresh_receiver = Some(rx.clone());
                let options = Arc::clone(&self.options);
                let state_clone = Arc::clone(&self.state);
                super::cleanup_task::spawn_with_error(
                    &tokio::runtime::Handle::current(),
                    tx,
                    PluginRuntimeError::new(
                        "Plugin refresh task was dropped or panicked before completion.",
                    ),
                    async move {
                        let result = perform_refresh(&options, &state_clone).await;
                        let mut s = state_clone.lock().await;
                        s.refresh_sender = None;
                        s.refresh_receiver = None;
                        drop(s);
                        result
                    },
                );
                rx
            }
        };
        wait_watch(rx).await
    }

    pub async fn lifecycle_state(&self) -> RegistryState {
        self.state.lock().await.registry.state
    }

    pub async fn generation(&self) -> u64 {
        self.state.lock().await.registry.current_generation()
    }

    pub async fn snapshot(&self) -> Vec<Arc<LoadedPlugin>> {
        self.state.lock().await.registry.list()
    }

    pub async fn create_view(
        &self,
        additional: Vec<Arc<LoadedPlugin>>,
    ) -> contributions::PluginRuntimeView {
        let mut plugins = self.state.lock().await.registry.list();
        plugins.extend(additional);
        contributions::PluginRuntimeView::new(plugins)
    }

    pub async fn acquire_command_catalog(
        &self,
    ) -> Result<PluginCommandCatalogLease, PluginRuntimeError> {
        let mut st = self.state.lock().await;
        let lease = st.registry.acquire()?;
        let snapshot =
            contributions::create_command_catalog_snapshot(lease.generation, &lease.plugins);
        let waiters = Arc::clone(&st.registry.waiters);
        Ok(PluginCommandCatalogLease {
            generation: lease.generation,
            contributions: snapshot,
            state: Arc::clone(&self.state),
            plugins: lease.plugins,
            released: false,
            release_receiver: None,
            release_result: None,
            waiters,
            executor: tokio::runtime::Handle::current(),
            failure: self.lease_failure.clone(),
        })
    }

    pub async fn acquire_session_contributions(
        &self,
        additional: Vec<Arc<LoadedPlugin>>,
    ) -> Result<PluginSessionContributionLease, PluginRuntimeError> {
        let mut st = self.state.lock().await;
        let lease = st.registry.acquire()?;
        let mut all_plugins = lease.plugins.clone();
        all_plugins.extend(additional);
        let snapshot =
            contributions::create_session_contribution_snapshot(lease.generation, &all_plugins);
        let waiters = Arc::clone(&st.registry.waiters);
        Ok(PluginSessionContributionLease {
            generation: lease.generation,
            contributions: snapshot,
            state: Arc::clone(&self.state),
            plugins: lease.plugins,
            released: false,
            release_receiver: None,
            release_result: None,
            waiters,
            executor: tokio::runtime::Handle::current(),
            failure: self.lease_failure.clone(),
        })
    }

    pub async fn acquire_contribution_snapshot(
        &self,
    ) -> Result<PluginContributionLease, PluginRuntimeError> {
        let mut st = self.state.lock().await;
        let lease = st.registry.acquire()?;
        let snapshot =
            contributions::create_contribution_snapshot(lease.generation, &lease.plugins);
        let waiters = Arc::clone(&st.registry.waiters);
        Ok(PluginContributionLease {
            generation: lease.generation,
            contributions: snapshot,
            state: Arc::clone(&self.state),
            plugins: lease.plugins,
            released: false,
            release_receiver: None,
            release_result: None,
            waiters,
            executor: tokio::runtime::Handle::current(),
            failure: self.lease_failure.clone(),
        })
    }

    pub async fn dispose(&self) -> Result<(), PluginRuntimeError> {
        // Failure must bypass even a contended state lock. The sticky channel
        // also closes registry admission without acquiring that lock in Drop.
        with_lease_failure(self.lease_failure.subscribe(), self.start_dispose()).await
    }

    async fn start_dispose(&self) -> Result<(), PluginRuntimeError> {
        let rx = {
            let mut st = self.state.lock().await;
            if let Some(rx) = &st.dispose_receiver {
                rx.clone()
            } else {
                // PluginRuntime.ts:263-272: close refresh admission, then wait for
                // the in-flight refresh before transitioning the registry.
                st.disposal_requested = true;
                let (tx, rx) = watch::channel(None);
                st.dispose_sender = Some(tx.clone());
                st.dispose_receiver = Some(rx.clone());
                let pending_refresh_rx = st.refresh_receiver.clone();
                let state_clone = Arc::clone(&self.state);
                let failure = self.lease_failure.subscribe();
                super::cleanup_task::spawn_with_error(
                    &tokio::runtime::Handle::current(),
                    tx,
                    PluginRuntimeError::new(
                        "Plugin disposal task was dropped or panicked before completion.",
                    ),
                    with_lease_failure(failure, async move {
                        if let Some(prx) = pending_refresh_rx {
                            let _ = wait_watch(prx).await;
                        }
                        let (pending, group) = {
                            let mut s = state_clone.lock().await;
                            s.registry.state = RegistryState::Draining;
                            let mut plugins = s.registry.retired_plugins.clone();
                            for plugin in s.registry.list() {
                                if !plugins.iter().any(|p| Arc::ptr_eq(p, &plugin)) {
                                    plugins.push(plugin);
                                }
                            }
                            s.registry.plugins.clear();
                            s.registry.order.clear();
                            s.registry.waiters.notify_waiters();
                            let group = start_disposals(&mut s.registry, plugins, &state_clone);
                            let pending = s
                                .registry
                                .pending_disposals
                                .iter()
                                .map(|pending| pending.receiver.clone())
                                .collect();
                            (pending, group)
                        };
                        let mut errors = settle_disposals(pending).await;
                        // PluginRegistry.ts:106-114 settles both the individual
                        // promises and disposeActive's group promise. Its group
                        // rejection is included even when it shares those causes.
                        if let Err(group) = PluginRuntimeError::aggregate(
                            "Failed to dispose retired plugins.",
                            settle_disposals(group).await,
                        ) {
                            errors.push(group);
                        }
                        state_clone.lock().await.registry.state = RegistryState::Disposed;
                        PluginRuntimeError::aggregate("Failed to dispose plugins.", errors)
                    }),
                );
                rx
            }
        };
        wait_watch(rx).await
    }

    pub async fn all_commands(&self) -> Vec<PluginCommandContribution> {
        contributions::collect_command_contributions(&self.state.lock().await.registry.list())
    }

    pub async fn all_skills(&self) -> Vec<PluginSkillContribution> {
        contributions::collect_skill_contributions(&self.state.lock().await.registry.list())
    }

    pub async fn mcp_servers(&self) -> JsObject {
        contributions::collect_mcp_servers(&self.state.lock().await.registry.list())
    }

    pub async fn lsp_servers(&self) -> JsObject {
        contributions::collect_lsp_servers(&self.state.lock().await.registry.list())
    }

    pub async fn all_mcp_instructions(&self) -> Vec<PluginMcpInstructionEntry> {
        contributions::collect_mcp_instructions(&self.state.lock().await.registry.list())
    }
}

// ─────────────────────────────────────────────────────────── perform_refresh

async fn perform_refresh(
    options: &PluginRuntimeOptions,
    state_mutex: &Arc<TokioMutex<PluginRuntimeState>>,
) -> Result<PluginRefreshResult, PluginRuntimeError> {
    let previous = state_mutex.lock().await.registry.list();
    let result = loader::load_from_disk(options).await;
    let mut state = state_mutex.lock().await;
    let keeps_old = result.staged_failures.iter().any(|failure| {
        previous
            .iter()
            .any(|p| p.source == failure.source && p.path == failure.path)
    });
    if keeps_old {
        let next = previous.clone();
        return Ok(PluginRefreshResult {
            generation: state.registry.current_generation(),
            previous,
            next,
            added: Vec::new(),
            removed: Vec::new(),
            staged_failures: result.staged_failures,
        });
    }
    let replacement = state.registry.replace_all(result.plugins)?;
    let pending = start_disposals(
        &mut state.registry,
        replacement.retired.clone(),
        state_mutex,
    );
    drop(state);
    if replacement.retiring_lease_count == 0 {
        let errors = settle_disposals(pending).await;
        PluginRuntimeError::aggregate("Failed to dispose retired plugins.", errors)?;
    }
    let added: Vec<_> = replacement
        .next
        .iter()
        .filter(|p| {
            !replacement
                .previous
                .iter()
                .any(|q| q.name == p.name && q.source == p.source)
        })
        .cloned()
        .collect();
    Ok(PluginRefreshResult {
        generation: replacement.generation,
        previous: replacement.previous,
        next: replacement.next,
        added,
        removed: replacement.removed,
        staged_failures: result.staged_failures,
    })
}

// ─────────────────────────────────────────────────────────── drain helpers

type DisposalReceiver = watch::Receiver<Option<Result<(), PluginRuntimeError>>>;

struct PendingDisposal {
    plugin: Arc<LoadedPlugin>,
    receiver: DisposalReceiver,
    // Taken exactly once when the instance becomes unreferenced.
    sender: Option<watch::Sender<Option<Result<(), PluginRuntimeError>>>>,
}

// Register every promise before starting callbacks. Keep JS Map insertion order
// even when a retired callback is still waiting for its lease (Registry:138-161).
fn start_disposals(
    registry: &mut PluginRegistry,
    plugins: Vec<Arc<LoadedPlugin>>,
    state_mutex: &Arc<TokioMutex<PluginRuntimeState>>,
) -> Vec<DisposalReceiver> {
    let woken = std::mem::take(&mut registry.disposal_waiters);
    let pending = plugins
        .into_iter()
        .map(|plugin| {
            if let Some(pending) = registry
                .pending_disposals
                .iter()
                .find(|pending| Arc::ptr_eq(&pending.plugin, &plugin))
            {
                return pending.receiver.clone();
            }
            let (tx, rx) = watch::channel(None);
            registry.disposal_waiters.push(Arc::clone(&plugin));
            registry.pending_disposals.push(PendingDisposal {
                plugin,
                receiver: rx.clone(),
                sender: Some(tx),
            });
            rx
        })
        .collect();
    registry.disposal_waiters.extend(woken);
    start_ready_disposals(registry, state_mutex);
    pending
}

// Reserve ready instances in TS waiter order while holding state. Never call
// third-party code here: both DisposeFn invocation and future polling may panic
// or re-enter the runtime. A separate start barrier serializes only prologues,
// not completion, so one pending callback cannot block its peers.
fn start_ready_disposals(
    registry: &mut PluginRegistry,
    state_mutex: &Arc<TokioMutex<PluginRuntimeState>>,
) {
    let mut ready = Vec::new();
    // New callbacks precede continuations woken by the lease notification.
    // Still-referenced continuations rejoin the end (Registry.ts:148,169).
    for plugin in std::mem::take(&mut registry.disposal_waiters) {
        if registry.is_plugin_referenced(&plugin) {
            registry.disposal_waiters.push(plugin);
            continue;
        }
        let pending = registry
            .pending_disposals
            .iter_mut()
            .find(|pending| Arc::ptr_eq(&pending.plugin, &plugin))
            .unwrap();
        let tx = pending.sender.take().unwrap();
        ready.push((plugin, tx));
    }
    if ready.is_empty() {
        return;
    }
    let previous_start = registry.disposal_start_receiver.take();
    let (started_tx, started_rx) = watch::channel(None);
    registry.disposal_start_receiver = Some(started_rx);
    // Build each per-plugin guard before the outer task runs: if the outer
    // future is dropped unpolled, every reserved disposal still settles.
    let ready: Vec<_> = ready
        .into_iter()
        .map(|(plugin, tx)| {
            (
                plugin,
                super::cleanup_task::Completion::new(
                    tx,
                    PluginRuntimeError::new(
                        "Plugin disposal task was dropped or panicked before completion.",
                    ),
                ),
            )
        })
        .collect();
    #[cfg(test)]
    let mut hook = registry.after_disposal_first_poll.take();
    let state = Arc::clone(state_mutex);
    super::cleanup_task::spawn_with_error(
        &tokio::runtime::Handle::current(),
        started_tx,
        PluginRuntimeError::new(
            "Plugin disposal start task was dropped or panicked before completion.",
        ),
        async move {
            if let Some(previous) = previous_start {
                let _ = wait_watch(previous).await;
            }
            let mut started = Vec::new();
            for (plugin, completion) in ready {
                let callback_plugin = Arc::clone(&plugin);
                let mut future = Box::pin(async move {
                    if let Some(dispose) = &callback_plugin.dispose {
                        dispose().await.map_err(PluginRuntimeError::new)?;
                    }
                    Ok(())
                });
                // TS's async IIFE owns synchronous throws as rejected promises
                // (PluginRegistry.ts:148-160). Catch both construction panics and
                // panics on any poll, so all peers still start and settle.
                let mut callback = Box::pin(std::future::poll_fn(move |cx| {
                    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        future.as_mut().poll(cx)
                    }))
                    .unwrap_or_else(|payload| {
                        let message = crate::runtime::cleanup_task::panic_message(payload.as_ref())
                            .unwrap_or("Plugin dispose callback panicked.");
                        std::task::Poll::Ready(Err(PluginRuntimeError::new(message)))
                    })
                }));
                // Keep this same pinned future: a notification delivered to the
                // noop waker remains in its state. The spawned task unconditionally
                // polls it once, then pending polls register the real task waker.
                let first = callback
                    .as_mut()
                    .poll(&mut std::task::Context::from_waker(std::task::Waker::noop()));
                #[cfg(test)]
                if let Some(hook) = hook.take() {
                    hook(first.is_pending());
                }
                started.push((plugin, completion, callback, first));
            }
            // Every synchronous prologue has run before any callback can resume.
            for (plugin, completion, callback, first) in started {
                let state = Arc::clone(&state);
                super::cleanup_task::spawn_completion(
                    &tokio::runtime::Handle::current(),
                    completion,
                    async move {
                        let result = match first {
                            std::task::Poll::Ready(result) => {
                                drop(callback);
                                result
                            }
                            std::task::Poll::Pending => callback.await,
                        };
                        let mut st = state.lock().await;
                        st.registry
                            .pending_disposals
                            .retain(|pending| !Arc::ptr_eq(&pending.plugin, &plugin));
                        st.registry
                            .retired_plugins
                            .retain(|p| !Arc::ptr_eq(p, &plugin));
                        result
                    },
                );
            }
            Ok(())
        },
    );
}

async fn settle_disposals(pending: Vec<DisposalReceiver>) -> Vec<PluginRuntimeError> {
    let mut errors = Vec::new();
    for rx in pending {
        if let Err(error) = wait_watch(rx).await {
            errors.push(error);
        }
    }
    errors
}

#[allow(clippy::collapsible_if)]
async fn wait_watch<T: Clone + Send + 'static>(mut rx: watch::Receiver<Option<T>>) -> T {
    loop {
        if let Some(value) = rx.borrow().clone() {
            return value;
        }
        // Err means the Sender was dropped (task panicked without sending).
        // Treat that as a permanent failure — re-check the value once (it may
        // have been set just before the drop), then panic with a clear message.
        if rx.changed().await.is_err() {
            if let Some(value) = rx.borrow().clone() {
                return value;
            }
            panic!(
                "wait_watch: watch sender dropped without delivering a value (spawned task panicked)"
            );
        }
    }
}

// Observe the current value before sleeping (wait_watch does both), including
// failure before subscription. The owned disposal task uses the same race so
// it cannot outlive its caller while waiting forever on a lost lease/refresh.
async fn with_lease_failure(
    failure: DisposalReceiver,
    cleanup: impl std::future::Future<Output = Result<(), PluginRuntimeError>>,
) -> Result<(), PluginRuntimeError> {
    tokio::select! {
        biased;
        failed = wait_watch(failure) => failed,
        result = cleanup => result,
    }
}

// ─────────────────────────────────────────────────────────── lease release helper

async fn release_lease_plugins(
    plugins: &[Arc<LoadedPlugin>],
    state_mutex: &Arc<TokioMutex<PluginRuntimeState>>,
    waiters: &Arc<Notify>,
    mut guard: contributions::LeaseReleaseGuard,
) -> Result<(), PluginRuntimeError> {
    let failure = guard.failure.subscribe();
    let pending = {
        let mut st = state_mutex.lock().await;
        for plugin in plugins {
            let key = Arc::as_ptr(plugin) as usize;
            if let Some(count) = st.registry.lease_counts.get_mut(&key) {
                if *count <= 1 {
                    st.registry.lease_counts.remove(&key);
                } else {
                    *count -= 1;
                }
            }
        }
        // No await between decrement and disarm: cancellation after this point
        // belongs to callback completion, not to a lost reference count.
        guard.counts_released();
        start_ready_disposals(&mut st.registry, state_mutex);
        plugins
            .iter()
            .filter_map(|p| {
                st.registry
                    .pending_disposals
                    .iter()
                    .find(|pending| Arc::ptr_eq(&pending.plugin, p))
                    .map(|pending| pending.receiver.clone())
            })
            .collect()
    };
    waiters.notify_waiters();
    with_lease_failure(failure, async {
        let errors = settle_disposals(pending).await;
        PluginRuntimeError::aggregate("Failed to dispose released plugins.", errors)
    })
    .await
}

// ─────────────────────────────────────────────────────────── CommandsListInput/Result

#[derive(Clone, Debug)]
pub struct CommandsListInput {
    pub project_key: String,
    pub query: Option<String>,
    pub cursor: Option<String>,
    pub limit: Option<u32>,
}

#[derive(Clone, Debug)]
pub struct CommandsListResult {
    pub value: pilotdeck_protocol::JsValue,
}

// ─────────────────────────────────────────────────────────── GatewayCommandCatalogBundle

/// Collaborator traits for `GatewayCommandCatalogBundle`.
pub trait CommandCatalogRuntime: Send + Sync {
    fn plugin_runtime(&self) -> Arc<PluginRuntime>;
}

pub type BoxCommandsListFuture =
    Pin<Box<dyn Future<Output = Result<CommandsListResult, PluginRuntimeError>> + Send + 'static>>;

pub trait CommandCatalogListCommands: Send + Sync {
    fn list(
        &self,
        input: CommandsListInput,
        pilot_home: String,
        commands: Vec<PluginCommandContribution>,
    ) -> BoxCommandsListFuture;
}

pub type WarnFn = Arc<dyn Fn(&str, &PluginRuntimeError) + Send + Sync>;

/// `GatewayCommandCatalogBundle` (TS lines 37-70).
///
/// `commandsList`: resolveProjectKey → resolveRuntime → refresh → acquire →
/// listCommands → finally release (release failure only warns).
pub struct GatewayCommandCatalogBundle {
    pilot_home: String,
    resolve_project_key: ResolveProjectKeyFn,
    resolve_runtime: Arc<dyn Fn(String) -> Arc<PluginRuntime> + Send + Sync>,
    list_commands: Arc<dyn CommandCatalogListCommands>,
    warn: WarnFn,
}

impl GatewayCommandCatalogBundle {
    pub fn new(
        pilot_home: String,
        resolve_project_key: ResolveProjectKeyFn,
        resolve_runtime: Arc<dyn Fn(String) -> Arc<PluginRuntime> + Send + Sync>,
        list_commands: Arc<dyn CommandCatalogListCommands>,
        warn: Option<WarnFn>,
    ) -> Self {
        Self {
            pilot_home,
            resolve_project_key,
            resolve_runtime,
            list_commands,
            warn: warn.unwrap_or_else(|| {
                Arc::new(|msg, err| {
                    eprintln!("{msg} {err}");
                })
            }),
        }
    }

    pub async fn commands_list(
        &self,
        input: CommandsListInput,
    ) -> Result<CommandsListResult, PluginRuntimeError> {
        let project_key = (self.resolve_project_key)(input.project_key.clone()).await?;
        let runtime = (self.resolve_runtime)(project_key.clone());
        runtime.refresh().await?;
        let mut lease = runtime.acquire_command_catalog().await?;
        let commands = lease.contributions.commands.clone();
        let result = self
            .list_commands
            .list(
                CommandsListInput {
                    project_key,
                    ..input
                },
                self.pilot_home.clone(),
                commands,
            )
            .await;
        // finally: release, warn on failure but preserve the result.
        self.release_lease(&mut lease).await;
        result
    }

    async fn release_lease(&self, lease: &mut PluginCommandCatalogLease) {
        if let Err(error) = lease.release().await {
            (self.warn)(
                "[pilotdeck] failed to release extension command catalog lease:",
                &error,
            );
        }
    }
}

#[cfg(test)]
#[path = "plugins_tests.rs"]
mod tests;
