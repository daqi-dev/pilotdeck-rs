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

mod contributions;
pub mod loader;
mod options;

// ─────────────────────────────────────────────────────────── error

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PluginRuntimeError {
    pub message: String,
    pub causes: Vec<PluginRuntimeError>,
}

impl PluginRuntimeError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            causes: Vec::new(),
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
                causes: errors,
            }),
        }
    }
    /// AggregateError from JS: .errors[] at depth 1.
    pub fn aggregate_js(message: &str, causes: Vec<PluginRuntimeError>) -> PluginRuntimeError {
        PluginRuntimeError {
            message: message.into(),
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
    lease_counts: HashMap<usize, usize>,
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
            lease_counts: HashMap::new(),
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

    fn replace_all(&mut self, plugins: Vec<Arc<LoadedPlugin>>) -> ReplaceResult {
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
            self.retired_plugins.push(Arc::clone(p));
        }
        self.generation += 1;

        ReplaceResult {
            generation: self.generation,
            previous,
            next,
            removed,
            retired,
            retiring_lease_count,
        }
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
    dispose_sender: Option<watch::Sender<Option<Result<(), PluginRuntimeError>>>>,
    dispose_receiver: Option<watch::Receiver<Option<Result<(), PluginRuntimeError>>>>,
}

// ─────────────────────────────────────────────────────────── PluginRuntime

/// `PluginRuntime` — thread-safe owner of the current plugin generation.
pub struct PluginRuntime {
    options: Arc<PluginRuntimeOptions>,
    state: Arc<TokioMutex<PluginRuntimeState>>,
}

impl PluginRuntime {
    pub fn new(options: PluginRuntimeOptions) -> Self {
        Self {
            options: Arc::new(options),
            state: Arc::new(TokioMutex::new(PluginRuntimeState {
                registry: PluginRegistry::new(),
                refresh_sender: None,
                refresh_receiver: None,
                dispose_sender: None,
                dispose_receiver: None,
            })),
        }
    }

    pub async fn refresh(&self) -> Result<Vec<Arc<LoadedPlugin>>, PluginRuntimeError> {
        Ok(self.refresh_with_report().await?.next)
    }

    /// Single-flight: concurrent callers share one pending refresh result.
    pub async fn refresh_with_report(&self) -> Result<PluginRefreshResult, PluginRuntimeError> {
        let rx = {
            let mut st = self.state.lock().await;
            if st.registry.state != RegistryState::Active {
                return Err(PluginRuntimeError::new(format!(
                    "Cannot refresh plugins; plugin runtime is {}.",
                    st.registry.state.as_str()
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
                tokio::spawn(async move {
                    let result = perform_refresh(&options, &state_clone).await;
                    let mut s = state_clone.lock().await;
                    s.refresh_sender = None;
                    s.refresh_receiver = None;
                    drop(s);
                    let _ = tx.send(Some(result));
                });
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
            waiters,
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
            waiters,
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
            waiters,
        })
    }

    pub async fn dispose(&self) -> Result<(), PluginRuntimeError> {
        let rx = {
            let mut st = self.state.lock().await;
            if let Some(rx) = &st.dispose_receiver {
                return wait_watch(rx.clone()).await;
            }
            st.registry.state = RegistryState::Draining;
            let plugins_to_drain: Vec<Arc<LoadedPlugin>> = {
                let active = st.registry.list();
                let retired = st.registry.retired_plugins.clone();
                let mut all = active;
                for p in retired {
                    if !all.iter().any(|a| Arc::ptr_eq(a, &p)) {
                        all.push(p);
                    }
                }
                st.registry.plugins.clear();
                st.registry.order.clear();
                st.registry.waiters.notify_waiters();
                all
            };
            let (tx, rx) = watch::channel(None);
            st.dispose_sender = Some(tx.clone());
            st.dispose_receiver = Some(rx.clone());
            let pending_refresh_rx = st.refresh_receiver.clone();
            let state_clone = Arc::clone(&self.state);
            tokio::spawn(async move {
                if let Some(prx) = pending_refresh_rx {
                    let _ =
                        wait_watch::<Result<PluginRefreshResult, PluginRuntimeError>>(prx).await;
                }
                let errors = drain_plugins(plugins_to_drain, &state_clone).await;
                {
                    let mut s = state_clone.lock().await;
                    s.registry.state = RegistryState::Disposed;
                    s.registry.waiters.notify_waiters();
                }
                let result = PluginRuntimeError::aggregate("Failed to dispose plugins.", errors);
                let _ = tx.send(Some(result));
            });
            rx
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
    let replacement = state.registry.replace_all(result.plugins);
    let retiring = replacement.retired.clone();
    let retiring_lease_count = replacement.retiring_lease_count;
    let state_clone = Arc::clone(state_mutex);
    if retiring_lease_count == 0 {
        drop(state);
        let errors = drain_plugins(retiring, &state_clone).await;
        PluginRuntimeError::aggregate("Failed to dispose retired plugins.", errors)?;
    } else {
        drop(state);
        tokio::spawn(async move {
            let _ = drain_plugins(retiring, &state_clone).await;
        });
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

async fn drain_plugins(
    plugins: Vec<Arc<LoadedPlugin>>,
    state_mutex: &Arc<TokioMutex<PluginRuntimeState>>,
) -> Vec<PluginRuntimeError> {
    let mut errors = Vec::new();
    for plugin in plugins {
        if let Err(e) = drain_one_plugin(plugin, state_mutex).await {
            errors.push(e);
        }
    }
    errors
}

async fn drain_one_plugin(
    plugin: Arc<LoadedPlugin>,
    state_mutex: &Arc<TokioMutex<PluginRuntimeState>>,
) -> Result<(), PluginRuntimeError> {
    loop {
        // `Notify::notified()` only registers the waiter when the future is
        // first polled, not when it is created.  We must call `.enable()` while
        // still holding the lock so the waiter is registered before we release
        // the guard.  This closes the race with `release_lease_plugins`
        // calling `notify_waiters()` between the lock release and the .await.
        // Pattern mirrors config/store.rs `wait_done` / `run_or_wait`.
        //
        // Clone the Arc<Notify> first so the Notified future borrows from the
        // local `waiters`, not from the MutexGuard — the guard can then drop
        // while the pinned future stays valid.
        let st = state_mutex.lock().await;
        if !st.registry.is_plugin_referenced(&plugin) {
            drop(st);
            break;
        }
        let waiters = Arc::clone(&st.registry.waiters);
        let notified = waiters.notified();
        tokio::pin!(notified);
        notified.as_mut().enable();
        drop(st);
        notified.await;
    }
    if let Some(dispose) = &plugin.dispose {
        dispose().await.map_err(PluginRuntimeError::new)?;
    }
    Ok(())
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

// ─────────────────────────────────────────────────────────── lease release helper

pub(super) async fn release_lease_plugins(
    plugins: &[Arc<LoadedPlugin>],
    state_mutex: &Arc<TokioMutex<PluginRuntimeState>>,
    waiters: &Arc<Notify>,
) {
    {
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
    }
    waiters.notify_waiters();
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
