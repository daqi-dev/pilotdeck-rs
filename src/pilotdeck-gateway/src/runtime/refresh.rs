//! Config/extension refresh coordination, ported from the fixed TypeScript
//! baseline's `src/cli/GatewayRuntimeRefreshBundle.ts` (lines 45-164).
//!
//! The bundle owns no state of its collaborators: it subscribes to the config
//! store, and turns each published change into the baseline's fixed call
//! sequence. Collaborators are injected as traits; [`ConfigStore`] and
//! [`ProjectRuntimeRegistry`] (through [`RegistryRefresh`]) are the product
//! implementations, the router / SDK callback / notification server /
//! memory maintenance belong to later ports.
//!
//! A published config change (`onConfigChange`) is handled synchronously in
//! the store's listener, exactly in TS order:
//!
//! 1. no change classes: nothing;
//! 2. every class `restart-required`: one `warn`, nothing else;
//! 3. run-policy-only paths: `log` → SDK callback → `config_changed`
//!    broadcast (no runtime reload);
//! 4. otherwise `log` → SDK callback → `registry.reload()`. When that reload
//!    settles: success runs router lookup → `runtime_invalidated` memory
//!    diagnostic → `markAllDirty("config_changed")` → broadcast; failure only
//!    warns and keeps the previous runtimes. A registry that refuses the
//!    reload synchronously (TS `assertActive` throw inside the listener,
//!    swallowed by the store) ends the handling silently.
//!
//! Settlement callbacks run in settlement order, like promise reactions: two
//! changes whose reloads settle out of order are broadcast out of order. The
//! baseline registry queues reloads at call time (`reloadTail`);
//! [`RegistryRefresh`] keeps that call-order queue, so with the product
//! registry the callbacks follow the change order on any Tokio runtime.
//!
//! The notification server is read when it is used: a server bound or
//! cleared (`dispose`) while a reload is pending decides whether that
//! reload's broadcast happens, as in TS. Strings stay UTF-16 (`JsString`)
//! end to end, so project keys and changed paths with lone surrogates reach
//! the collaborators and log lines unchanged.
use super::project::{ProjectRuntimeRegistry, RuntimeError, RuntimeResult};
use crate::config::{
    Error as ConfigError,
    changes::{ChangeClass, is_run_policy_only_change},
    store::{ConfigStore, ReloadEvent},
};
use pilotdeck_protocol::{JsObject, JsString, JsValue};
use std::{
    collections::BTreeSet,
    future::Future,
    path::Path,
    pin::Pin,
    sync::{Arc, Mutex, MutexGuard},
};
use tokio::{runtime::Handle, sync::watch};

pub type BoxFuture<T> = Pin<Box<dyn Future<Output = T> + Send + 'static>>;
/// A config-store subscriber: the event's `changedPaths` and `changeClasses`.
pub type ConfigListener = Arc<dyn Fn(&[JsString], &[ChangeClass]) + Send + Sync>;
/// The unsubscribe function returned by [`RefreshConfigStore::subscribe`].
pub type Unsubscribe = Box<dyn FnOnce() + Send>;
pub type ConfigReloadResult = Result<(), Arc<ConfigError>>;
/// `getRouter`: the router may not exist yet (it is created after the bundle).
pub type GetRouter = Arc<dyn Fn() -> Option<Arc<dyn RefreshRouter>> + Send + Sync>;

/// The `configStore` option.
pub trait RefreshConfigStore: Send + Sync {
    fn subscribe(&self, listener: ConfigListener) -> Unsubscribe;
    /// The reload is claimed when this is called; the returned future settles
    /// it. Errors are the store's (shared) reload error.
    fn reload(&self, reason: &'static str) -> BoxFuture<ConfigReloadResult>;
}

impl RefreshConfigStore for ConfigStore {
    fn subscribe(&self, listener: ConfigListener) -> Unsubscribe {
        let subscription = ConfigStore::subscribe(
            self,
            Arc::new(move |event: &ReloadEvent| {
                listener(&event.changed_paths, &event.change_classes)
            }),
        );
        Box::new(move || subscription.unsubscribe())
    }
    fn reload(&self, reason: &'static str) -> BoxFuture<ConfigReloadResult> {
        let reload = ConfigStore::reload(self, reason);
        Box::pin(async move { reload.await.map(|_| ()) })
    }
}

/// The `registry` option.
pub trait RefreshRegistry: Send + Sync {
    /// `Err` is a synchronous refusal (the baseline throws before queueing);
    /// `Ok` is the queued reload. The bundle drops the future only after it
    /// ran that reload's settlement callbacks, so an implementation may keep
    /// later reloads queued until the drop — the baseline's next queued
    /// reload also starts only after earlier promise reactions ran.
    fn reload(&self) -> Result<BoxFuture<RuntimeResult>, RuntimeError>;
    /// `invalidate(projectKey)`; `None` is the argument-less call.
    fn invalidate(&self, project_key: Option<&JsString>);
}

/// `GatewayRuntimeRefreshRouter`.
pub trait RefreshRouter: Send + Sync {
    fn mark_all_dirty(&self, reason: &str) -> usize;
    fn mark_project_dirty(&self, project_key: &JsString, reason: &str) -> usize;
    /// `SessionRouter.markSessionDirty`: unlike the bulk marks, a missing
    /// session is a no-op and reports `false`.
    fn mark_session_dirty(&self, session_key: &JsString, reason: &str) -> bool;
    fn cached_session_count(&self) -> usize;
    /// `snapshotSession(sessionKey)?.messages`.
    fn snapshot_session(&self, session_key: &JsString) -> Option<JsValue>;
}

/// `GatewayNotificationServer`.
pub trait NotificationServer: Send + Sync {
    fn broadcast_notification(&self, name: &str, payload: &JsValue);
}

/// `dispatchSdkConfigChange`.
pub trait SdkConfigChange: Send + Sync {
    fn dispatch_sdk_config_change(&self, change: &ConfigChangeNotice);
}

/// `ProjectMemoryMaintenancePort`.
pub trait MemoryMaintenance: Send + Sync {
    fn schedule(&self, project_key: &JsString);
}

/// `logMemoryDiagnostic` and `summarizeMessages`; present only when memory
/// diagnostics are enabled (`memoryDiagnosticsEnabled`).
pub trait MemoryDiagnostics: Send + Sync {
    fn log_memory_diagnostic(&self, input: JsValue);
    fn summarize_messages(&self, messages: &JsValue) -> JsObject;
}

/// The second `warn` argument: the joined paths, or the rejection.
#[derive(Clone, Copy, Debug)]
pub enum WarnDetail<'a> {
    Text(&'a JsString),
    Error(&'a RuntimeError),
}

/// The `log` / `warn` options.
pub trait RefreshLog: Send + Sync {
    fn log(&self, message: &JsString);
    fn warn(&self, message: &str, detail: WarnDetail<'_>);
}

/// The baseline's `console.log` / `console.warn` defaults.
pub struct ConsoleLog;
impl RefreshLog for ConsoleLog {
    fn log(&self, message: &JsString) {
        println!("{}", String::from_utf16_lossy(message.units()));
    }
    fn warn(&self, message: &str, detail: WarnDetail<'_>) {
        match detail {
            WarnDetail::Text(text) => {
                eprintln!("{message} {}", String::from_utf16_lossy(text.units()))
            }
            WarnDetail::Error(error) => eprintln!("{message} {error}"),
        }
    }
}

/// `{ changedPaths, changeClasses }` as given to the SDK callback and
/// broadcast with `config_changed`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConfigChangeNotice {
    pub changed_paths: Vec<JsString>,
    pub change_classes: Vec<JsString>,
}
impl ConfigChangeNotice {
    pub fn to_js(&self) -> JsValue {
        let strings = |items: &[JsString]| {
            JsValue::Array(items.iter().cloned().map(JsValue::String).collect())
        };
        let mut object = JsObject::new();
        object.insert("changedPaths".into(), strings(&self.changed_paths));
        object.insert("changeClasses".into(), strings(&self.change_classes));
        JsValue::Object(object)
    }
}

/// `{ reloaded: true, changedPaths }`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReloadOutcome {
    pub reloaded: bool,
    pub changed_paths: Vec<JsString>,
}

/// `reloadExtensions` input.
#[derive(Clone, Debug, Default)]
pub struct ExtensionsReload {
    pub project_key: Option<JsString>,
    pub changed_paths: Option<Vec<JsString>>,
}

/// `afterTurnCompleted` input.
#[derive(Clone, Debug)]
pub struct TurnCompleted {
    pub session_key: JsString,
    pub project_key: Option<JsString>,
    pub run_id: Option<JsString>,
}

/// `GatewayRuntimeRefreshBundleOptions`.
pub struct RefreshOptions {
    pub config_store: Arc<dyn RefreshConfigStore>,
    pub registry: Arc<dyn RefreshRegistry>,
    pub memory_maintenance: Arc<dyn MemoryMaintenance>,
    pub router: GetRouter,
    pub project_root: JsString,
    pub memory_diagnostics: Option<Arc<dyn MemoryDiagnostics>>,
    pub sdk: Option<Arc<dyn SdkConfigChange>>,
    pub log: Arc<dyn RefreshLog>,
}

struct Inner {
    options: RefreshOptions,
    unsubscribe: Mutex<Option<Unsubscribe>>,
    server: Mutex<Option<Arc<dyn NotificationServer>>>,
    executor: Handle,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// `GatewayRuntimeRefreshBundle`. Construct inside a Tokio runtime; settled
/// runtime reloads are handled on that runtime.
pub struct RefreshBundle {
    inner: Arc<Inner>,
}

impl RefreshBundle {
    pub fn new(options: RefreshOptions) -> Self {
        Self {
            inner: Arc::new(Inner {
                options,
                unsubscribe: Mutex::new(None),
                server: Mutex::new(None),
                executor: Handle::current(),
            }),
        }
    }

    /// `attach`: subscribe once.
    ///
    /// Like the baseline closure `(event) => this.onConfigChange(event)`,
    /// the subscription holds the bundle strongly: dropping every
    /// `RefreshBundle` handle without [`dispose`](Self::dispose) keeps
    /// handling config changes (TS keeps working once the last reference is
    /// dropped too) rather than silently going dead behind a stale listener.
    /// `dispose` is the only detach and releases that reference.
    pub fn attach(&self) {
        let mut unsubscribe = lock(&self.inner.unsubscribe);
        if unsubscribe.is_some() {
            return;
        }
        let inner = Arc::clone(&self.inner);
        *unsubscribe = Some(self.inner.options.config_store.subscribe(Arc::new(
            move |paths: &[JsString], classes: &[ChangeClass]| {
                on_config_change(&inner, paths, classes);
            },
        )));
    }

    /// `dispose`: unsubscribe and forget the bound server. Pending runtime
    /// reloads still settle (and mark the router dirty) but broadcast nothing.
    pub fn dispose(&self) {
        let unsubscribe = lock(&self.inner.unsubscribe).take();
        if let Some(unsubscribe) = unsubscribe {
            unsubscribe();
        }
        *lock(&self.inner.server) = None;
    }

    /// `bindServer`.
    pub fn bind_server(&self, server: Arc<dyn NotificationServer>) {
        *lock(&self.inner.server) = Some(server);
    }

    /// `reloadConfig`: an RPC reload reporting the changed paths of the last
    /// event published while it runs (none: `[]`). The capture listener is
    /// subscribed and the reload claimed when this is called; it is removed
    /// when the reload settles or the future is dropped.
    pub fn reload_config(
        &self,
    ) -> impl Future<Output = Result<ReloadOutcome, Arc<ConfigError>>> + Send + 'static {
        struct Capture(Option<Unsubscribe>);
        impl Drop for Capture {
            fn drop(&mut self) {
                if let Some(unsubscribe) = self.0.take() {
                    unsubscribe();
                }
            }
        }
        let captured = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&captured);
        let store = &self.inner.options.config_store;
        let capture = Capture(Some(store.subscribe(Arc::new(
            move |paths: &[JsString], _: &[ChangeClass]| *lock(&sink) = paths.to_vec(),
        ))));
        let reload = store.reload("rpc");
        async move {
            let result = reload.await;
            drop(capture);
            result?;
            let changed_paths = std::mem::take(&mut *lock(&captured));
            Ok(ReloadOutcome {
                reloaded: true,
                changed_paths,
            })
        }
    }

    /// `reloadExtensions`. The baseline method has no await, so everything
    /// happens synchronously here.
    pub fn reload_extensions(&self, input: ExtensionsReload) -> ReloadOutcome {
        let options = &self.inner.options;
        let changed_paths = input.changed_paths.unwrap_or_default();
        let router = (options.router)();
        let mut shown = join(&changed_paths);
        if shown.is_empty() {
            shown = units("(manual)");
        }
        // `if (input?.projectKey)`: an empty key is the all-runtimes branch.
        match input.project_key.filter(|key| !key.is_empty()) {
            Some(key) => {
                let mut message = units("[pilotdeck] Extensions reload requested for project ");
                message.extend_from_slice(key.units());
                message.extend(units(": "));
                message.extend(shown);
                options.log.log(&JsString::from_utf16(message));
                options.registry.invalidate(Some(&key));
                if let Some(router) = router {
                    router.mark_project_dirty(&key, "extension_changed");
                }
            }
            None => {
                let mut message =
                    units("[pilotdeck] Extensions reload requested for all runtimes: ");
                message.extend(shown);
                options.log.log(&JsString::from_utf16(message));
                options.registry.invalidate(None);
                if let Some(router) = router {
                    router.mark_all_dirty("extension_changed");
                }
            }
        }
        broadcast(
            &self.inner,
            &ConfigChangeNotice {
                changed_paths: changed_paths.clone(),
                change_classes: vec!["extension-changed".into()],
            },
        );
        ReloadOutcome {
            reloaded: true,
            changed_paths,
        }
    }

    /// `refreshConfigBeforeTurn`.
    pub fn refresh_config_before_turn(
        &self,
    ) -> impl Future<Output = ConfigReloadResult> + Send + 'static {
        self.inner.options.config_store.reload("turn-start")
    }

    /// `afterTurnCompleted`; synchronous like the baseline body.
    pub fn after_turn_completed(&self, input: TurnCompleted) {
        let options = &self.inner.options;
        if let Some(diagnostics) = &options.memory_diagnostics {
            let snapshot =
                (options.router)().and_then(|router| router.snapshot_session(&input.session_key));
            let session_count =
                count((options.router)().map(|router| router.cached_session_count()));
            let mut session = JsObject::new();
            session.insert(
                "sessionKey".into(),
                JsValue::String(input.session_key.clone()),
            );
            session.insert("projectKey".into(), optional(&input.project_key));
            session.insert("runId".into(), optional(&input.run_id));
            if let Some(messages) = snapshot {
                // Object spread: own keys, later ones replace in place. Only
                // string-key order is kept by insertion; integer-like keys are
                // ordered by every JS-order consumer (stringify) either way.
                for (key, value) in diagnostics.summarize_messages(&messages).entries() {
                    session.insert(key.clone(), value.clone());
                }
            }
            let mut record = JsObject::new();
            record.insert("event".into(), "turn_completed".into());
            record.insert("sessionCount".into(), session_count);
            record.insert("session".into(), JsValue::Object(session));
            diagnostics.log_memory_diagnostic(JsValue::Object(record));
        }
        // `projectKey ?? projectRoot`: an empty key is kept.
        let project_key = input.project_key.as_ref().unwrap_or(&options.project_root);
        options.memory_maintenance.schedule(project_key);
    }
}

fn units(text: &str) -> Vec<u16> {
    text.encode_utf16().collect()
}

/// `changedPaths.join(", ")`.
fn join(paths: &[JsString]) -> Vec<u16> {
    let mut out = Vec::new();
    for (index, path) in paths.iter().enumerate() {
        if index > 0 {
            out.extend(units(", "));
        }
        out.extend_from_slice(path.units());
    }
    out
}

fn optional(value: &Option<JsString>) -> JsValue {
    value.clone().map_or(JsValue::Undefined, JsValue::String)
}

fn count(value: Option<usize>) -> JsValue {
    value.map_or(JsValue::Undefined, |n| JsValue::Number(n as f64))
}

fn broadcast(inner: &Inner, notice: &ConfigChangeNotice) {
    // Read at use time and never call out under the lock.
    let server = lock(&inner.server).clone();
    if let Some(server) = server {
        server.broadcast_notification("config_changed", &notice.to_js());
    }
}

fn on_config_change(inner: &Arc<Inner>, paths: &[JsString], classes: &[ChangeClass]) {
    let options = &inner.options;
    if classes.is_empty() {
        return;
    }
    let joined = JsString::from_utf16(join(paths));
    if classes
        .iter()
        .all(|class| *class == ChangeClass::RestartRequired)
    {
        options.log.warn(
            "[pilotdeck] Config change requires process restart:",
            WarnDetail::Text(&joined),
        );
        return;
    }
    let notice = ConfigChangeNotice {
        changed_paths: paths.to_vec(),
        change_classes: classes.iter().map(|class| class.as_str().into()).collect(),
    };
    let log = |prefix: &str| {
        let mut message = units(prefix);
        message.extend_from_slice(joined.units());
        options.log.log(&JsString::from_utf16(message));
    };
    if is_run_policy_only_change(paths) {
        log("[pilotdeck] Config reloaded (runPolicy applies next turn): ");
        if let Some(sdk) = &options.sdk {
            sdk.dispatch_sdk_config_change(&notice);
        }
        broadcast(inner, &notice);
        return;
    }
    log("[pilotdeck] Config reloaded, refreshing runtimes: ");
    if let Some(sdk) = &options.sdk {
        sdk.dispatch_sdk_config_change(&notice);
    }
    // TS: a synchronous throw propagates into the store, which swallows it.
    let Ok(reload) = options.registry.reload() else {
        return;
    };
    // Like the baseline's closure, the pending reload keeps the bundle alive.
    let inner = Arc::clone(inner);
    let executor = inner.executor.clone();
    // #333: a Tokio runtime that has shut down drops a spawned future without
    // polling it (measured on 1.48: `Handle::spawn` does not panic). TS has
    // no such state; rather than lose the refresh silently, the guard warns
    // when the settlement never ran.
    let lost = SettlementGuard {
        inner: Some(Arc::clone(&inner)),
        joined,
    };
    executor.spawn(async move {
        let mut lost = lost;
        // The reload future is dropped only after its settlement callbacks:
        // a registry may hold its queue position until then (see
        // [`RefreshRegistry::reload`]).
        let mut reload = reload;
        let settled = reload.as_mut().await;
        lost.inner = None;
        match settled {
            Ok(()) => {
                let options = &inner.options;
                let router = (options.router)();
                if let Some(diagnostics) = &options.memory_diagnostics {
                    let mut record = JsObject::new();
                    record.insert("event".into(), "runtime_invalidated".into());
                    record.insert(
                        "sessionCount".into(),
                        count(router.as_ref().map(|router| router.cached_session_count())),
                    );
                    record.insert(
                        "projectKey".into(),
                        JsValue::String(options.project_root.clone()),
                    );
                    record.insert("reason".into(), "config_changed".into());
                    diagnostics.log_memory_diagnostic(JsValue::Object(record));
                }
                if let Some(router) = router {
                    router.mark_all_dirty("config_changed");
                }
                broadcast(&inner, &notice);
            }
            Err(error) => inner.options.log.warn(
                "[pilotdeck] Config runtime refresh rejected; keeping previous runtimes:",
                WarnDetail::Error(&error),
            ),
        }
        drop(reload);
    });
}

/// Warns once if a queued settlement is dropped before its reload settled
/// (executor shut down, or the task cancelled with it).
struct SettlementGuard {
    inner: Option<Arc<Inner>>,
    joined: JsString,
}

impl Drop for SettlementGuard {
    fn drop(&mut self) {
        if let Some(inner) = self.inner.take() {
            inner.options.log.warn(
                "[pilotdeck] Config runtime refresh lost; the runtime executor stopped before the reload settled:",
                WarnDetail::Text(&self.joined),
            );
        }
    }
}

/// [`RefreshRegistry`] over the product [`ProjectRuntimeRegistry`].
///
/// The baseline registry queues a reload when `reload()` is *called*; the
/// Rust registry's gate is only taken when its future is first polled, and
/// spawned futures may be polled in any order. Each call here takes a ticket
/// synchronously, and its reload starts only once every earlier ticket's
/// future has been dropped (after completion and the bundle's settlement
/// callbacks, or abandoned unpolled), which restores call order and the
/// TS order "first reload's callbacks, then the next reload".
pub struct RegistryRefresh {
    registry: Arc<ProjectRuntimeRegistry>,
    queue: Arc<TicketQueue>,
}

impl RegistryRefresh {
    pub fn new(registry: Arc<ProjectRuntimeRegistry>) -> Self {
        Self {
            registry,
            queue: Arc::new(TicketQueue {
                state: Mutex::new(TicketState::default()),
                turn: watch::channel(0).0,
            }),
        }
    }
}

#[derive(Default)]
struct TicketState {
    next: u64,
    turn: u64,
    finished: BTreeSet<u64>,
}

struct TicketQueue {
    state: Mutex<TicketState>,
    turn: watch::Sender<u64>,
}

/// Releases a queue position when dropped.
struct Ticket {
    queue: Arc<TicketQueue>,
    id: u64,
}

impl Drop for Ticket {
    fn drop(&mut self) {
        let mut state = lock(&self.queue.state);
        state.finished.insert(self.id);
        let mut turn = state.turn;
        while state.finished.remove(&turn) {
            turn += 1;
        }
        state.turn = turn;
        self.queue.turn.send_replace(turn);
    }
}

/// Holds its ticket until dropped, not merely until ready.
struct Queued {
    _ticket: Ticket,
    reload: BoxFuture<RuntimeResult>,
}

impl Future for Queued {
    type Output = RuntimeResult;
    fn poll(
        self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<RuntimeResult> {
        self.get_mut().reload.as_mut().poll(cx)
    }
}

impl RefreshRegistry for RegistryRefresh {
    fn reload(&self) -> Result<BoxFuture<RuntimeResult>, RuntimeError> {
        if self.registry.is_disposed() {
            return Err(RuntimeError::new("Project runtime registry is disposed."));
        }
        let ticket = {
            let mut state = lock(&self.queue.state);
            let id = state.next;
            state.next += 1;
            Ticket {
                queue: Arc::clone(&self.queue),
                id,
            }
        };
        let mut turn = self.queue.turn.subscribe();
        let registry = Arc::clone(&self.registry);
        let id = ticket.id;
        Ok(Box::pin(Queued {
            _ticket: ticket,
            reload: Box::pin(async move {
                // The sender lives in the ticket's queue, so this cannot close
                // early.
                let _ = turn.wait_for(|turn| *turn >= id).await;
                registry.reload().await
            }),
        }))
    }

    fn invalidate(&self, project_key: Option<&JsString>) {
        match project_key {
            None => self.registry.invalidate(None),
            // Registry keys are UTF-8 paths, so a key with a lone surrogate
            // equals none of them (a JS Map lookup that misses).
            Some(key) => {
                if let Ok(key) = key.to_utf8() {
                    self.registry.invalidate(Some(Path::new(&key)));
                }
            }
        }
    }
}

#[cfg(test)]
#[path = "refresh_tests.rs"]
mod tests;
