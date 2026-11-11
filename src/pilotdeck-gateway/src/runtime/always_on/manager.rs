//! Port of TS `src/always-on/runtime/AlwaysOnRuntime.ts` and
//! `AlwaysOnManager.ts` (baseline cc7c4e6) — the per-project lifecycle owner
//! and the multi-project coordinator behind the control port.
//!
//! The runtime owns a scheduler per project and answers the three control
//! entries (`applyCycle` / `abortRun` / `rerunPlan`,
//! AlwaysOnRuntime.ts:317-404). `stop` is the matrix §三 r3 shape
//! (#2260): the persisted `stopPromise` idempotency, `stopping` flipping
//! every control entry to `not_ready` *before* the drain, the scheduler join
//! (which itself waits out an in-flight tick), and `Promise.allSettled` over
//! the active control runs — their rejections are dropped without a log
//! (AlwaysOnRuntime.ts:296-315). An abort on an in-flight run is never
//! issued: `stop` only waits.
//!
//! The two halves of the TS runtime that belong to the deep pipeline stay
//! unported behind the `fire.rs` seam:
//! - `AlwaysOnRunContextRegistry` is ported (abort's ownership check reads
//!   it, AlwaysOnRuntime.ts:375-385), but its only writer in TS is the fire
//!   pipeline registering a context before submitting a turn — the seam
//!   never does, so production registers nothing and `abortRun` answers
//!   `session_not_active` for every key.
//! - `SessionConfigOverrides` and its `deletePrefix` cleanup in `stop`
//!   (AlwaysOnRuntime.ts:309-311) are the same story: the fire pipeline is
//!   the only writer. Not ported; recorded in the -R1 ledger.
//!
//! The manager (AlwaysOnManager.ts:72-227) shares one run-context registry
//! across one `AlwaysOnRuntime` per enabled project and routes the control
//! entries by `projectKey`.
use std::{
    collections::HashMap,
    future::Future,
    io,
    path::{Path, PathBuf},
    pin::Pin,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};

use pilotdeck_protocol::{JsObject, JsString, JsValue};

use super::{
    AlwaysOnConfig, AlwaysOnLogger, AlwaysOnPaths, ChannelLeaseRegistry, DiscoveryPlanStore,
    DiscoveryScheduler, DiscoveryStateStore, IsProjectBusy, SchedulerDeps, WorkCycleStore,
    fire::{
        ApplyPhaseInput, DiscoveryFire, DiscoveryFirePort, RerunFireInput, derive_apply_session_key,
    },
};

/// The `error` field of every control result
/// (`src/always-on/protocol/AlwaysOnControlPort.ts`): a machine `code` plus
/// a human `message`.
#[derive(Clone, Debug)]
pub(crate) struct ControlError {
    pub code: String,
    pub message: String,
}

/// `AlwaysOnAgentGatewayPort` (AlwaysOnAgentGatewayPort.ts:8-24) as the ported
/// halves see it. The full TS port also carries `submitTurn` (an event
/// iterable) and `closeSession`; both are only called by the fire pipeline's
/// deep phases, which stay behind the `fire.rs` seam — so the port's surface
/// is the one method the control half really calls, `abortTurn`
/// (AlwaysOnRuntime.ts:387-392). `submitTurn`'s telemetry shape and the
/// fire-and-forget `closeSession` land with that later lane.
pub(crate) struct GatewayAbortInput<'a> {
    pub session_key: &'a str,
    pub run_id: Option<&'a str>,
    pub reason: Option<&'a str>,
}

/// One unattended phase submitted through the existing native session owner.
#[derive(Clone)]
pub(crate) struct PhaseRequest {
    pub project_key: JsString,
    pub session_key: JsString,
    pub channel_key: JsString,
    pub run_id: JsString,
    pub message: JsString,
    pub cwd: PathBuf,
    pub permission_mode: JsString,
    pub bypass_available: bool,
    pub can_prompt: bool,
    pub exclude_tools: Vec<JsString>,
    pub permission_deny: Vec<crate::config::permission::PermissionRule>,
    pub context_registry: Arc<AlwaysOnRunContextRegistry>,
    pub register_automation_tools: bool,
    pub observer: Arc<dyn PhaseObserver>,
}

/// Bounded ordered turn events, including the terminal event. `error` is the
/// first error event; phase-specific missing codes are filled by the caller.
pub(crate) struct PhaseResult {
    pub events: Vec<JsValue>,
    pub error: Option<ControlError>,
    completion: Option<Box<dyn PhaseCompletion>>,
}
impl PhaseResult {
    pub(crate) fn new(events: Vec<JsValue>, error: Option<ControlError>) -> Self {
        Self {
            events,
            error,
            completion: None,
        }
    }
    pub(crate) fn with_completion(mut self, completion: Box<dyn PhaseCompletion>) -> Self {
        self.completion = Some(completion);
        self
    }
    /// Fire emits its completed/failed event before releasing the session.
    /// An abandoned receipt still owns cleanup through the implementation's Drop.
    pub(crate) async fn finish(&mut self) {
        if let Some(completion) = self.completion.take() {
            completion.close().await;
        }
    }
}
pub(crate) trait PhaseCompletion: Send {
    fn close(self: Box<Self>) -> Pin<Box<dyn Future<Output = ()> + Send>>;
}
pub(crate) trait PhaseObserver: Send + Sync {
    fn on_event<'a>(
        &'a self,
        session: &'a JsString,
        channel: &'a JsString,
        phase_run: &'a JsString,
        event: &'a JsValue,
    ) -> Pin<Box<dyn Future<Output = ()> + Send + 'a>>;
}

/// The abort half of the turn facade. The error is the `Error.message` the TS
/// catch reads (`error instanceof Error ? error.message : String(error)`,
/// AlwaysOnRuntime.ts:399-400).
pub(crate) trait AlwaysOnAgentGateway: Send + Sync {
    fn run_phase<'a>(
        &'a self,
        request: PhaseRequest,
    ) -> Pin<Box<dyn Future<Output = Result<PhaseResult, ControlError>> + Send + 'a>>;

    fn abort_turn<'a>(
        &'a self,
        input: GatewayAbortInput<'a>,
    ) -> Pin<Box<dyn Future<Output = Result<(), JsString>> + Send + 'a>>;
}

/// One `AlwaysOnRunContext` as the ported halves read it: `abortRun` claims
/// a session through `projectKey`/`runId` (AlwaysOnRuntime.ts:375-385), and
/// `stop` unregisters this project's contexts (AlwaysOnRuntime.ts:304-308).
/// The `kind`-specific fields (plan stores, handles, call counts) belong to
/// the fire pipeline's contexts and stay unported with it.
#[derive(Clone, Debug)]
pub(crate) struct AlwaysOnRunContext {
    pub session_key: String,
    pub run_id: String,
    pub project_key: String,
}

/// `AlwaysOnRunContextRegistry` (AlwaysOnRunContextRegistry.ts:78-119):
/// single-process `sessionKey` → in-flight run context map. `register`
/// refuses a duplicate key exactly as the TS throw does
/// (AlwaysOnRunContextRegistry.ts:82-84).
pub(crate) struct AlwaysOnRunContextRegistry {
    pub(super) contexts: Mutex<HashMap<String, AlwaysOnRunContext>>,
    pub(super) tool_handlers: Mutex<HashMap<JsString, Arc<super::tools::PhaseHandlerEntry>>>,
}
impl AlwaysOnRunContextRegistry {
    pub(crate) fn new() -> Self {
        Self {
            contexts: Mutex::new(HashMap::new()),
            tool_handlers: Mutex::new(HashMap::new()),
        }
    }

    pub(crate) fn register(&self, context: AlwaysOnRunContext) -> Result<(), io::Error> {
        let mut contexts = self.contexts.lock().unwrap_or_else(|e| e.into_inner());
        if contexts.contains_key(&context.session_key) {
            return Err(io::Error::other(format!(
                "AlwaysOn run context already exists for sessionKey: {}",
                context.session_key
            )));
        }
        contexts.insert(context.session_key.clone(), context);
        Ok(())
    }

    /// The run-end unregister the TS registry performs; the caller lands
    /// with the explicitly unported fire pipeline.
    #[allow(dead_code)]
    pub(crate) fn unregister(&self, session_key: &str) {
        let mut contexts = self.contexts.lock().unwrap_or_else(|e| e.into_inner());
        contexts.remove(session_key);
        let mut handlers = self.tool_handlers.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(entry) = handlers.remove(&JsString::from(session_key)) {
            *entry.active.lock().unwrap_or_else(|e| e.into_inner()) = false;
        }
    }

    pub(crate) fn get(&self, session_key: &str) -> Option<AlwaysOnRunContext> {
        self.contexts
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(session_key)
            .cloned()
    }

    /// The project-scoped sweep of `stop` (AlwaysOnRuntime.ts:304-308).
    pub(crate) fn unregister_project(&self, project_key: &str) {
        let mut contexts = self.contexts.lock().unwrap_or_else(|e| e.into_inner());
        let sessions: Vec<JsString> = contexts
            .iter()
            .filter(|(_, context)| context.project_key == project_key)
            .map(|(session, _)| JsString::from(session.as_str()))
            .collect();
        contexts.retain(|_, context| context.project_key != project_key);
        let mut handlers = self.tool_handlers.lock().unwrap_or_else(|e| e.into_inner());
        for session in sessions {
            if let Some(entry) = handlers.remove(&session) {
                *entry.active.lock().unwrap_or_else(|e| e.into_inner()) = false;
            }
        }
    }
}

/// `AlwaysOnApplyInput` / `AlwaysOnAbortInput` / `AlwaysOnRerunPlanInput`
/// (AlwaysOnControlPort.ts:8-40). The optional fields are the TS optionals; a
/// field missing on the wire is rendered into the string fields at the
/// routing boundary with JS `String()` semantics.
pub(crate) struct ApplyInput {
    pub project_key: String,
    pub work_cycle_id: String,
    pub project_name: Option<JsString>,
}

pub(crate) struct AbortInput {
    pub project_key: String,
    pub session_key: String,
    pub reason: Option<String>,
}

pub(crate) struct RerunInput {
    pub project_key: String,
    pub plan_id: String,
}

pub(crate) struct ApplyResult {
    pub session_key: String,
    pub error: Option<ControlError>,
}

pub(crate) struct AbortResult {
    pub aborted: bool,
    pub session_key: String,
    pub run_id: Option<String>,
    pub error: Option<ControlError>,
}

pub(crate) struct RerunResult {
    pub run_id: String,
    pub error: Option<ControlError>,
}

/// `Date.now()` idiom the port's clocks share (epoch ms).
fn now_millis() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as f64)
        .unwrap_or(0.0)
}

fn not_ready_apply() -> ApplyResult {
    ApplyResult {
        session_key: String::new(),
        error: Some(ControlError {
            code: "not_ready".into(),
            message: "AlwaysOnRuntime.bindGateway not called".into(),
        }),
    }
}

fn not_ready_abort(session_key: &str) -> AbortResult {
    AbortResult {
        aborted: false,
        session_key: session_key.into(),
        run_id: None,
        error: Some(ControlError {
            code: "not_ready".into(),
            message: "AlwaysOnRuntime.bindGateway not called".into(),
        }),
    }
}

fn not_ready_rerun() -> RerunResult {
    RerunResult {
        run_id: String::new(),
        error: Some(ControlError {
            code: "not_ready".into(),
            message: "AlwaysOnRuntime.bindGateway not called".into(),
        }),
    }
}

/// One `ActiveControlRun` (AlwaysOnRuntime.ts:110-114): the run's id, the
/// session keys it claims (what `abortRun` matches against), and the
/// completion signal `stop`'s `Promise.allSettled` joins — a `watch` pair,
/// so a run that finishes between the stop's snapshot and its await is seen
/// as finished rather than lost (a bare `Notify` would miss it).
struct ActiveControlRun {
    session_keys: Vec<String>,
    finished: tokio::sync::watch::Receiver<bool>,
}

struct RuntimeShared {
    agent_gateway: Mutex<Option<Arc<dyn AlwaysOnAgentGateway>>>,
    fire: Mutex<Option<Arc<dyn DiscoveryFirePort>>>,
    scheduler: Mutex<Option<Arc<DiscoveryScheduler>>>,
    /// `stopping` (AlwaysOnRuntime.ts:162): set the moment `stop` begins, so
    /// every control entry answers `not_ready` for the whole drain, not only
    /// after it.
    stopping: AtomicBool,
    stop_started: AtomicBool,
    /// #2486: liveness marker for an in-flight `stop` drain. TS's
    /// `stopPromise` runs to completion once called; the port's `stop`
    /// future can be dropped at any await (the assembly's single-poll
    /// builder drops the whole start chain, failure cleanup included), which
    /// would leave `stop_started` latched with nobody left to send
    /// `stop_finished`. The dropped drain's guard sets this, so the next
    /// `stop` re-runs the drain instead of waiting on a signal that will
    /// never come.
    stop_abandoned: AtomicBool,
    stop_finished: tokio::sync::watch::Sender<bool>,
    /// #2486: holds the channel's initial receiver, so a `stop_finished`
    /// send always records — a `watch` send with zero receivers fails
    /// without storing, which left a completed drain's settlement
    /// invisible to a later sequential stop (and to the re-run of an
    /// abandoned drain). Holding a receiver is the field's whole job, so
    /// it is intentionally never read (fire.rs 的 `started_at_ms` 同款
    /// `#[allow(dead_code)]` 口径).
    #[allow(dead_code)]
    stop_finished_hold: tokio::sync::watch::Receiver<bool>,
    stop_error: Mutex<Option<String>>,
    active_control_runs: Mutex<HashMap<String, ActiveControlRun>>,
}

/// Dependencies bound to the existing per-project runtime generation.
pub(crate) struct FireInputs {
    pub config: AlwaysOnConfig,
    pub project_key: String,
    pub paths: AlwaysOnPaths,
    pub state_store: Arc<DiscoveryStateStore>,
    pub plan_store: Arc<DiscoveryPlanStore>,
    pub cycle_store: Arc<WorkCycleStore>,
    pub contexts: Arc<AlwaysOnRunContextRegistry>,
    pub gateway: Arc<dyn AlwaysOnAgentGateway>,
    pub now: Arc<dyn Fn() -> f64 + Send + Sync>,
    pub uuid: Arc<dyn Fn() -> String + Send + Sync>,
}
pub(crate) trait FireFactory: Send + Sync {
    fn create(&self, input: FireInputs) -> io::Result<Arc<dyn DiscoveryFirePort>>;
}

/// #2486: the drain's drop guard — armed when the drain body begins,
/// disarmed only at its `stop_finished` send. A drop in between marks the
/// drain abandoned for the next `stop` to re-run (the body is idempotent:
/// the scheduler slot and the run map are drained with `take`, and the
/// remaining clears are plain overwrites).
struct StopDrainGuard {
    shared: Arc<RuntimeShared>,
    armed: bool,
}

impl StopDrainGuard {
    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for StopDrainGuard {
    fn drop(&mut self) {
        if self.armed {
            self.shared.stop_abandoned.store(true, Ordering::SeqCst);
        }
    }
}

/// `AlwaysOnRuntime` (AlwaysOnRuntime.ts:133-448): the lifecycle owner. The
/// construction sequence is the TS one (constructor → `bindAgentGateway` →
/// `start` → `stop`), with the same refusal messages on out-of-order calls.
pub(crate) struct AlwaysOnRuntime {
    config: AlwaysOnConfig,
    project_key: String,
    paths: AlwaysOnPaths,
    state_store: Arc<DiscoveryStateStore>,
    plan_store: Arc<DiscoveryPlanStore>,
    cycle_store: Arc<WorkCycleStore>,
    leases: Arc<ChannelLeaseRegistry>,
    run_contexts: Arc<AlwaysOnRunContextRegistry>,
    logger: Arc<dyn AlwaysOnLogger>,
    now: Arc<dyn Fn() -> f64 + Send + Sync>,
    uuid: Arc<dyn Fn() -> String + Send + Sync>,
    is_session_in_flight: Arc<dyn Fn() -> bool + Send + Sync>,
    shared: Arc<RuntimeShared>,
}

impl AlwaysOnRuntime {
    /// The TS constructor's storage resolution
    /// (AlwaysOnRuntime.ts:169-173): the native per-project paths under the
    /// pilot home, keyed by the storage id. A path resolution failure is the
    /// TS constructor throw. Construction is effect-free — the scheduler is
    /// only created at `bindAgentGateway` time (AlwaysOnRuntime.ts:244-277).
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        config: AlwaysOnConfig,
        pilot_home: &Path,
        project_key: &str,
        run_contexts: Option<Arc<AlwaysOnRunContextRegistry>>,
        now: Option<Arc<dyn Fn() -> f64 + Send + Sync>>,
        uuid: Option<Arc<dyn Fn() -> String + Send + Sync>>,
        logger: Option<Arc<dyn AlwaysOnLogger>>,
        is_session_in_flight: Option<Arc<dyn Fn() -> bool + Send + Sync>>,
    ) -> io::Result<Self> {
        let metadata = crate::metadata::Metadata {
            home: pilot_home.to_path_buf(),
            cwd: PathBuf::new(),
            files: crate::metadata::NativeFiles,
        };
        let paths = super::resolve_always_on_paths(pilot_home, project_key, &metadata)?;
        Self::new_with_paths(
            config,
            paths,
            run_contexts,
            now,
            uuid,
            logger,
            is_session_in_flight,
        )
    }

    /// Native callers supply paths resolved by the bounded marker adapter.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new_with_paths(
        config: AlwaysOnConfig,
        paths: AlwaysOnPaths,
        run_contexts: Option<Arc<AlwaysOnRunContextRegistry>>,
        now: Option<Arc<dyn Fn() -> f64 + Send + Sync>>,
        uuid: Option<Arc<dyn Fn() -> String + Send + Sync>>,
        logger: Option<Arc<dyn AlwaysOnLogger>>,
        is_session_in_flight: Option<Arc<dyn Fn() -> bool + Send + Sync>>,
    ) -> io::Result<Self> {
        let now: Arc<dyn Fn() -> f64 + Send + Sync> = now.unwrap_or_else(|| Arc::new(now_millis));
        let leases = Arc::new(ChannelLeaseRegistry::new(now.clone()));
        let state_store = Arc::new(DiscoveryStateStore::new(paths.clone()));
        let plan_store = Arc::new(DiscoveryPlanStore::new(paths.clone()));
        let cycle_store = Arc::new(WorkCycleStore::new(paths.clone()));
        let (stop_finished, stop_finished_hold) = tokio::sync::watch::channel(false);
        Ok(Self {
            project_key: paths.project_key.clone(),
            config,
            paths,
            state_store,
            plan_store,
            cycle_store,
            leases,
            run_contexts: run_contexts
                .unwrap_or_else(|| Arc::new(AlwaysOnRunContextRegistry::new())),
            logger: logger.unwrap_or_else(|| Arc::new(super::NoopAlwaysOnLogger)),
            now,
            uuid: uuid.unwrap_or_else(|| Arc::new(|| uuid::Uuid::new_v4().to_string())),
            is_session_in_flight: is_session_in_flight.unwrap_or_else(|| Arc::new(|| false)),
            shared: Arc::new(RuntimeShared {
                agent_gateway: Mutex::new(None),
                fire: Mutex::new(None),
                scheduler: Mutex::new(None),
                stopping: AtomicBool::new(false),
                stop_started: AtomicBool::new(false),
                stop_abandoned: AtomicBool::new(false),
                stop_finished,
                stop_error: Mutex::new(None),
                stop_finished_hold,
                active_control_runs: Mutex::new(HashMap::new()),
            }),
        })
    }

    /// `projectKey` as the manager routes by it (AlwaysOnManager.ts:186,194,206).
    pub(crate) fn project_key(&self) -> &str {
        &self.project_key
    }

    /// `getRunContexts` (AlwaysOnRuntime.ts:231-233) — the probe's registration
    /// route for the abort-ownership scenarios.
    pub(crate) fn run_contexts(&self) -> &Arc<AlwaysOnRunContextRegistry> {
        &self.run_contexts
    }

    /// `bindAgentGateway` (AlwaysOnRuntime.ts:235-278): the double-bind
    /// refusal, then the fire and the scheduler. The scheduler's
    /// `isSessionInFlight` hook is the TS resolution — the bind hook when
    /// present, else the constructor's own.
    pub(crate) fn bind_agent_gateway(
        &self,
        gateway: Arc<dyn AlwaysOnAgentGateway>,
        is_session_in_flight: Option<Arc<dyn Fn() -> bool + Send + Sync>>,
    ) -> Result<(), io::Error> {
        self.bind_with_factory(gateway, is_session_in_flight, None)
    }

    fn bind_with_factory(
        &self,
        gateway: Arc<dyn AlwaysOnAgentGateway>,
        is_session_in_flight: Option<Arc<dyn Fn() -> bool + Send + Sync>>,
        factory: Option<&Arc<dyn FireFactory>>,
    ) -> Result<(), io::Error> {
        let mut bound = self
            .shared
            .agent_gateway
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if bound.is_some() {
            return Err(io::Error::other(
                "AlwaysOnRuntime.bindAgentGateway already called.",
            ));
        }
        let fire: Arc<dyn DiscoveryFirePort> = if let Some(factory) = factory {
            factory.create(FireInputs {
                config: self.config.clone(),
                project_key: self.project_key.clone(),
                paths: self.paths.clone(),
                state_store: self.state_store.clone(),
                plan_store: self.plan_store.clone(),
                cycle_store: self.cycle_store.clone(),
                contexts: self.run_contexts.clone(),
                gateway: gateway.clone(),
                now: self.now.clone(),
                uuid: self.uuid.clone(),
            })?
        } else {
            Arc::new(DiscoveryFire::new(self.plan_store.clone()))
        };
        *bound = Some(gateway);
        let in_flight = is_session_in_flight.unwrap_or_else(|| self.is_session_in_flight.clone());
        let scheduler = Arc::new(DiscoveryScheduler::new(SchedulerDeps {
            config: self.config.clone(),
            project_key: self.project_key.clone(),
            paths: self.paths.clone(),
            state_store: self.state_store.clone(),
            cycle_store: self.cycle_store.clone(),
            leases: self.leases.clone(),
            fire: fire.clone(),
            uuid: self.uuid.clone(),
            now: self.now.clone(),
            logger: self.logger.clone(),
            is_session_in_flight: in_flight,
        }));
        *self.shared.fire.lock().unwrap_or_else(|e| e.into_inner()) = Some(fire);
        *self
            .shared
            .scheduler
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = Some(scheduler);
        Ok(())
    }

    /// `start` (AlwaysOnRuntime.ts:288-294): the bind gate first, then the
    /// scheduler, then the log line.
    pub(crate) async fn start(&self) -> Result<(), io::Error> {
        let scheduler = self
            .shared
            .scheduler
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let Some(scheduler) = scheduler else {
            return Err(io::Error::other(
                "AlwaysOnRuntime.start called before bindGateway.",
            ));
        };
        scheduler.start().await?;
        self.log_info("always-on runtime started");
        Ok(())
    }

    /// `stop` (AlwaysOnRuntime.ts:296-315): the persisted `stopPromise`
    /// idempotency, the drain, and the project-scoped context sweep. An
    /// in-flight control run is waited out — its rejection is dropped without
    /// a log, the `allSettled` shape; nothing is ever aborted.
    /// #2486: TS's `stopPromise` is never dropped mid-flight; the port's
    /// stop future can be (the assembly's single-poll builder drops the
    /// whole start chain at the failure cleanup's first yield), so a later
    /// stop re-runs an abandoned drain rather than waiting on its
    /// `stop_finished` — the compensating stop the builder's host teardown
    /// performs must complete, not hang off a dead first drain.
    pub(crate) async fn stop(&self) {
        let _ = self.stop_checked().await;
    }

    pub(crate) async fn stop_checked(&self) -> io::Result<()> {
        if self.shared.stop_started.swap(true, Ordering::SeqCst)
            && !self.shared.stop_abandoned.swap(false, Ordering::SeqCst)
        {
            let mut finished = self.shared.stop_finished.subscribe();
            while !*finished.borrow_and_update() {
                if finished.changed().await.is_err() {
                    break;
                }
            }
            return self
                .shared
                .stop_error
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .as_ref()
                .map_or(Ok(()), |message| Err(io::Error::other(message.clone())));
        }
        let mut drain = StopDrainGuard {
            shared: Arc::clone(&self.shared),
            armed: true,
        };
        self.shared.stopping.store(true, Ordering::SeqCst);
        // The guard is a temporary of the `if let` condition, which would
        // otherwise live across the await below and make this future
        // non-`Send`; the hoisted `let` drops it at the end of the statement.
        let scheduler = self
            .shared
            .scheduler
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take();
        if let Some(scheduler) = scheduler {
            scheduler.stop().await;
        }
        let runs: Vec<ActiveControlRun> = std::mem::take(
            &mut *self
                .shared
                .active_control_runs
                .lock()
                .unwrap_or_else(|e| e.into_inner()),
        )
        .into_values()
        .collect();
        for run in runs {
            let mut finished = run.finished;
            while !*finished.borrow_and_update() {
                if finished.changed().await.is_err() {
                    break;
                }
            }
        }
        let fire = self
            .shared
            .fire
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take();
        let error = if let Some(fire) = fire {
            fire.close().await.err().map(|e| e.to_string())
        } else {
            None
        };
        self.run_contexts.unregister_project(&self.project_key);
        self.log_info("always-on runtime stopped");
        *self
            .shared
            .stop_error
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = error.clone();
        self.shared.stop_finished.send_replace(true);
        drain.disarm();
        error.map_or(Ok(()), |message| Err(io::Error::other(message)))
    }

    /// `rerunPlan` (AlwaysOnRuntime.ts:317-331).
    pub(crate) async fn rerun_plan(&self, input: &RerunInput) -> io::Result<RerunResult> {
        let fire = self
            .shared
            .fire
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let Some(fire) = fire.filter(|_| !self.shared.stopping.load(Ordering::SeqCst)) else {
            return Ok(not_ready_rerun());
        };
        let run_id = (self.uuid)();
        // `sessionKeysForRerun` (AlwaysOnRuntime.ts:423-429).
        let session_keys = vec![
            super::derive_workspace_session_key(&self.project_key, &run_id),
            super::derive_execution_session_key(&self.project_key, &run_id),
            super::derive_report_session_key(&self.project_key, &run_id),
        ];
        let plan_id = input.plan_id.clone();
        let now = self.now.clone();
        let fire_run_id = run_id.clone();
        self.track_control_run(run_id, session_keys, not_ready_rerun, async move {
            let started_at_ms = (now)();
            let result = fire
                .rerun_plan(RerunFireInput {
                    plan_id: &plan_id,
                    run_id: &fire_run_id,
                    started_at_ms,
                })
                .await?;
            Ok(RerunResult {
                run_id: result.run_id,
                error: result.error,
            })
        })
        .await
    }

    /// `applyCycle` (AlwaysOnRuntime.ts:333-363): the cycle lookup, the plan
    /// projection, then the fire seam.
    pub(crate) async fn apply_cycle(&self, input: &ApplyInput) -> io::Result<ApplyResult> {
        let fire = self
            .shared
            .fire
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let Some(fire) = fire.filter(|_| !self.shared.stopping.load(Ordering::SeqCst)) else {
            return Ok(not_ready_apply());
        };
        let run_id = (self.uuid)();
        let session_keys = vec![derive_apply_session_key(&self.project_key, &run_id)];
        let work_cycle_id = input.work_cycle_id.clone();
        let project_name = input.project_name.clone();
        let project_root = input.project_key.clone();
        let cycle_store = self.cycle_store.clone();
        let fire_run_id = run_id.clone();
        self.track_control_run(run_id, session_keys, not_ready_apply, async move {
            let Some((cycle, plans)) = cycle_store
                .read_apply(work_cycle_id.as_str().into())
                .await?
            else {
                return Ok(ApplyResult {
                    session_key: String::new(),
                    error: Some(ControlError {
                        code: "cycle_not_found".into(),
                        message: format!("Work cycle {work_cycle_id} not found"),
                    }),
                });
            };
            let result = fire
                .run_apply_phase(ApplyPhaseInput {
                    run_id: &fire_run_id,
                    cycle: &cycle,
                    plans: &plans,
                    project_name: project_name.as_ref(),
                    project_root: &project_root,
                })
                .await?;
            Ok(ApplyResult {
                session_key: result.session_key,
                error: result.error,
            })
        })
        .await
    }

    /// `abortRun` (AlwaysOnRuntime.ts:365-404).
    pub(crate) async fn abort_run(&self, input: &AbortInput) -> AbortResult {
        let gateway = self
            .shared
            .agent_gateway
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let fire = self
            .shared
            .fire
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let stopping = self.shared.stopping.load(Ordering::SeqCst);
        let Some(gateway) = gateway.filter(|_| fire.is_some() && !stopping) else {
            return not_ready_abort(&input.session_key);
        };

        let context = self.run_contexts.get(&input.session_key);
        let control_run = self
            .shared
            .active_control_runs
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .find(|(_, run)| run.session_keys.iter().any(|key| key == &input.session_key))
            .map(|(run_id, _)| run_id.clone());
        let owned = context
            .as_ref()
            .is_some_and(|context| context.project_key == self.project_key);
        if !owned && control_run.is_none() {
            return AbortResult {
                aborted: false,
                session_key: input.session_key.clone(),
                run_id: None,
                error: Some(ControlError {
                    code: "session_not_active".into(),
                    message: "No active Always-On run owns this session.".into(),
                }),
            };
        }
        let run_id = context.map(|context| context.run_id).or(control_run);
        let reason = input
            .reason
            .clone()
            .unwrap_or_else(|| "always-on/abort_requested".into());
        match gateway
            .abort_turn(GatewayAbortInput {
                session_key: &input.session_key,
                run_id: run_id.as_deref(),
                reason: Some(&reason),
            })
            .await
        {
            Ok(()) => AbortResult {
                aborted: true,
                session_key: input.session_key.clone(),
                run_id,
                error: None,
            },
            Err(message) => AbortResult {
                aborted: false,
                session_key: input.session_key.clone(),
                run_id,
                error: Some(ControlError {
                    code: "abort_failed".into(),
                    message: String::from_utf16_lossy(message.units()),
                }),
            },
        }
    }

    /// `trackControlRun` (AlwaysOnRuntime.ts:406-421): the entry registers
    /// before the run and removes itself after. The run body runs as its own
    /// task — the promise shape — so a caller that drops its request future
    /// mid-run cannot strand the entry a concurrent `stop` would then wait on
    /// forever; the task finishes, signals, and removes itself. The
    /// completion signal is what `stop`'s `allSettled` joins; a run rejected
    /// with an io failure crosses as `Err` to its caller (the TS rejection)
    /// but is only *awaited out*, never logged, by a concurrent `stop`.
    async fn track_control_run<T: Send + 'static>(
        &self,
        run_id: String,
        session_keys: Vec<String>,
        not_ready: fn() -> T,
        run: impl Future<Output = io::Result<T>> + Send + 'static,
    ) -> io::Result<T> {
        let (finished_tx, finished) = tokio::sync::watch::channel(false);
        {
            let mut active = self
                .shared
                .active_control_runs
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            // Stop publishes `stopping` before taking this same registry.
            // Admission either precedes its drain or observes the closed gate.
            if self.shared.stopping.load(Ordering::SeqCst) {
                return Ok(not_ready());
            }
            active.insert(
                run_id.clone(),
                ActiveControlRun {
                    session_keys,
                    finished,
                },
            );
        }
        let shared = self.shared.clone();
        let task = tokio::spawn(async move {
            let result = run.await;
            let _ = finished_tx.send(true);
            shared
                .active_control_runs
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .remove(&run_id);
            result
        });
        match task.await {
            Ok(result) => result,
            // A panic in the run body: the TS promise would reject with the
            // panic value; the port surfaces it as an io failure with the
            // same reach (the caller's Err, `stop`'s silent await-out).
            Err(join_error) => Err(io::Error::other(join_error.to_string())),
        }
    }

    /// Test-only seam swap: replaces the bound fire so a control run can be
    /// parked inside its fire await (the stop-waits cell).
    #[cfg(test)]
    fn swap_fire_for_test(&self, fire: Arc<dyn DiscoveryFirePort>) {
        *self.shared.fire.lock().unwrap_or_else(|e| e.into_inner()) = Some(fire);
    }

    fn log_info(&self, message: &str) {
        let mut meta = JsObject::new();
        meta.insert(
            "projectKey".into(),
            JsValue::String(JsString::from(self.project_key.clone())),
        );
        self.logger.info(message, &meta);
    }
}

/// `CreateAlwaysOnManagerOptions` (AlwaysOnManager.ts:34-57) as the port needs
/// it: the config, the home the per-project storage roots under, and the
/// clock/logger seams.
pub(crate) struct AlwaysOnManagerOptions {
    pub config: AlwaysOnConfig,
    pub pilot_home: PathBuf,
    pub now: Option<Arc<dyn Fn() -> f64 + Send + Sync>>,
    pub uuid: Option<Arc<dyn Fn() -> String + Send + Sync>>,
    pub logger: Option<Arc<dyn AlwaysOnLogger>>,
}

/// `AlwaysOnManager` (AlwaysOnManager.ts:72-227): one runtime per enabled
/// project, one shared run-context registry, and the `projectKey` routing of
/// the three control entries.
pub struct AlwaysOnManager {
    runtimes: Vec<Arc<AlwaysOnRuntime>>,
    logger: Arc<dyn AlwaysOnLogger>,
    factory_state: Mutex<ManagerFactoryState>,
}
#[derive(Default)]
struct ManagerFactoryState {
    factory: Option<Arc<dyn FireFactory>>,
    binding_started: bool,
}

impl AlwaysOnManager {
    /// The constructor loop (AlwaysOnManager.ts:110-133): one runtime per
    /// **enabled** project, in config order, sharing the registry. A path
    /// resolution failure for any project is the TS constructor throw.
    pub(crate) fn new(options: AlwaysOnManagerOptions) -> io::Result<Self> {
        let run_contexts = Arc::new(AlwaysOnRunContextRegistry::new());
        let mut runtimes = Vec::new();
        for (project_key, enabled) in &options.config.projects {
            if !*enabled {
                continue;
            }
            runtimes.push(Arc::new(AlwaysOnRuntime::new(
                options.config.clone(),
                &options.pilot_home,
                project_key,
                Some(run_contexts.clone()),
                options.now.clone(),
                options.uuid.clone(),
                options.logger.clone(),
                None,
            )?));
        }
        Ok(Self {
            runtimes,
            factory_state: Mutex::new(ManagerFactoryState::default()),
            logger: options
                .logger
                .unwrap_or_else(|| Arc::new(super::NoopAlwaysOnLogger)),
        })
    }

    /// Native construction resolves every enabled project's paths through its
    /// bounded marker adapter before constructing the shared manager.
    pub(crate) fn new_with_resolver(
        options: AlwaysOnManagerOptions,
        mut resolve: impl FnMut(&str) -> io::Result<AlwaysOnPaths>,
    ) -> io::Result<Self> {
        let run_contexts = Arc::new(AlwaysOnRunContextRegistry::new());
        let mut runtimes = Vec::new();
        for (project_key, enabled) in &options.config.projects {
            if !*enabled {
                continue;
            }
            let paths = resolve(project_key)?;
            runtimes.push(Arc::new(AlwaysOnRuntime::new_with_paths(
                options.config.clone(),
                paths,
                Some(run_contexts.clone()),
                options.now.clone(),
                options.uuid.clone(),
                options.logger.clone(),
                None,
            )?));
        }
        Ok(Self {
            runtimes,
            factory_state: Mutex::new(ManagerFactoryState::default()),
            logger: options
                .logger
                .unwrap_or_else(|| Arc::new(super::NoopAlwaysOnLogger)),
        })
    }

    /// Install the native fire factory once, before any gateway bind.
    pub(crate) fn set_fire_factory(&self, factory: Arc<dyn FireFactory>) -> io::Result<()> {
        let mut state = self.factory_state.lock().unwrap_or_else(|e| e.into_inner());
        if state.binding_started || state.factory.is_some() {
            return Err(io::Error::other(
                "Always-On fire factory is already installed or binding started",
            ));
        }
        state.factory = Some(factory);
        Ok(())
    }

    /// `bindAgentGateway` (AlwaysOnManager.ts:147-160): every runtime binds
    /// the one facade; when the busy hook is present each runtime's
    /// `isSessionInFlight` closes over its own project key.
    pub(crate) fn bind_agent_gateway(
        &self,
        gateway: Arc<dyn AlwaysOnAgentGateway>,
        is_project_busy: Option<IsProjectBusy>,
    ) -> Result<(), io::Error> {
        let factory = {
            let mut state = self.factory_state.lock().unwrap_or_else(|e| e.into_inner());
            if state.binding_started {
                return Err(io::Error::other(
                    "Always-On gateway binding already started",
                ));
            }
            state.binding_started = true;
            state.factory.clone()
        };
        for runtime in &self.runtimes {
            let in_flight = is_project_busy.as_ref().map(|busy| {
                let busy = busy.clone();
                let project_key = runtime.project_key().to_string();
                let hook: Arc<dyn Fn() -> bool + Send + Sync> =
                    Arc::new(move || busy(&project_key));
                hook
            });
            runtime.bind_with_factory(gateway.clone(), in_flight, factory.as_ref())?;
        }
        Ok(())
    }

    /// `start` (AlwaysOnManager.ts:170-177).
    pub(crate) async fn start(&self) -> Result<(), io::Error> {
        for runtime in &self.runtimes {
            runtime.start().await?;
        }
        if self.runtimes.is_empty() {
            // AlwaysOnManager.ts:174-176 — no metadata object on this call
            // site; the port's logger signature carries an empty one, which
            // the recording logger renders as `{}` where TS renders
            // `undefined`. Not golden-pinned; noted in the -R1 ledger.
            self.logger.info(
                "always-on manager: no enabled projects; nothing to start.",
                &JsObject::new(),
            );
        }
        Ok(())
    }

    /// `stop` (AlwaysOnManager.ts:179-183).
    pub(crate) async fn stop(&self) -> io::Result<()> {
        let mut first = None;
        for runtime in &self.runtimes {
            if let Err(error) = runtime.stop_checked().await
                && first.is_none()
            {
                first = Some(error);
            }
        }
        first.map_or(Ok(()), Err)
    }

    fn runtime_for(&self, project_key: &str) -> Option<&Arc<AlwaysOnRuntime>> {
        self.runtimes
            .iter()
            .find(|runtime| runtime.project_key() == project_key)
    }

    /// `rerunPlan` (AlwaysOnManager.ts:185-191).
    pub(crate) async fn rerun_plan(&self, input: &RerunInput) -> io::Result<RerunResult> {
        let Some(runtime) = self.runtime_for(&input.project_key) else {
            return Ok(RerunResult {
                run_id: String::new(),
                error: Some(ControlError {
                    code: "project_not_found".into(),
                    message: format!("No Always-On runtime for project {}", input.project_key),
                }),
            });
        };
        runtime.rerun_plan(input).await
    }

    /// `applyCycle` (AlwaysOnManager.ts:193-203).
    pub(crate) async fn apply_cycle(&self, input: &ApplyInput) -> io::Result<ApplyResult> {
        let Some(runtime) = self.runtime_for(&input.project_key) else {
            return Ok(ApplyResult {
                session_key: String::new(),
                error: Some(ControlError {
                    code: "project_not_found".into(),
                    message: format!("No Always-On runtime for project {}", input.project_key),
                }),
            });
        };
        runtime.apply_cycle(input).await
    }

    /// `abortRun` (AlwaysOnManager.ts:205-215).
    pub(crate) async fn abort_run(&self, input: &AbortInput) -> AbortResult {
        let Some(runtime) = self.runtime_for(&input.project_key) else {
            return AbortResult {
                aborted: false,
                session_key: input.session_key.clone(),
                run_id: None,
                error: Some(ControlError {
                    code: "project_not_found".into(),
                    message: format!("No Always-On runtime for project {}", input.project_key),
                }),
            };
        };
        runtime.abort_run(input).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::negative_assert_eq;
    use std::path::{Path, PathBuf};

    const GOLDEN: &str = include_str!("../../../tests/fixtures/always-on-2294/oracle.json");
    const NOW_MS: f64 = 1_767_225_900_000.0;

    fn golden(id: &str) -> serde_json::Value {
        let records: Vec<serde_json::Value> =
            serde_json::from_str(GOLDEN).expect("#2294 golden parses");
        records
            .iter()
            .find(|record| record["id"].as_str() == Some(id))
            .map(|record| record["value"].clone())
            .unwrap_or_else(|| panic!("#2294 golden record {id} missing"))
    }

    fn scratch(tag: &str) -> PathBuf {
        let home = std::env::temp_dir().join(format!("always-on-2294-mgr-{tag}"));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(home.join("proj")).expect("#2294 fixture home");
        home
    }

    fn config(project_key: &str) -> AlwaysOnConfig {
        let mut config = AlwaysOnConfig::default_config();
        config.projects = vec![(project_key.into(), true)];
        config
    }

    fn counter_uuid(first: u32) -> (Arc<Mutex<u32>>, Arc<dyn Fn() -> String + Send + Sync>) {
        let counter = Arc::new(Mutex::new(first - 1));
        let seed = counter.clone();
        let uuid: Arc<dyn Fn() -> String + Send + Sync> = Arc::new(move || {
            let mut counter = seed.lock().unwrap_or_else(|e| e.into_inner());
            *counter += 1;
            format!("probe-run-{counter}")
        });
        (counter, uuid)
    }

    fn error_json(error: &Option<ControlError>) -> serde_json::Value {
        match error {
            Some(error) => serde_json::json!({"code": error.code, "message": error.message}),
            None => serde_json::Value::Null,
        }
    }

    fn apply_json(result: &ApplyResult) -> serde_json::Value {
        let mut json = serde_json::Map::new();
        json.insert("sessionKey".into(), serde_json::json!(result.session_key));
        if let Some(error) = &result.error {
            json.insert("error".into(), error_json(&Some(error.clone())));
        }
        serde_json::Value::Object(json)
    }

    fn abort_json(result: &AbortResult) -> serde_json::Value {
        let mut json = serde_json::Map::new();
        json.insert("aborted".into(), serde_json::json!(result.aborted));
        json.insert("sessionKey".into(), serde_json::json!(result.session_key));
        if let Some(run_id) = &result.run_id {
            json.insert("runId".into(), serde_json::json!(run_id));
        }
        if let Some(error) = &result.error {
            json.insert("error".into(), error_json(&Some(error.clone())));
        }
        serde_json::Value::Object(json)
    }

    fn rerun_json(result: &RerunResult) -> serde_json::Value {
        let mut json = serde_json::Map::new();
        json.insert("runId".into(), serde_json::json!(result.run_id));
        if let Some(error) = &result.error {
            json.insert("error".into(), error_json(&Some(error.clone())));
        }
        serde_json::Value::Object(json)
    }

    /// The probe's recording gateway (generate.mts:514-521): every call is
    /// recorded as `{op, input}` and every method succeeds.
    struct RecordingGateway {
        calls: Mutex<Vec<serde_json::Value>>,
    }
    impl AlwaysOnAgentGateway for RecordingGateway {
        fn run_phase<'a>(
            &'a self,
            _request: PhaseRequest,
        ) -> Pin<Box<dyn Future<Output = Result<PhaseResult, ControlError>> + Send + 'a>> {
            Box::pin(async {
                Err(ControlError {
                    code: "unexpected_phase".into(),
                    message: "A phase was unexpectedly submitted by this fixture".into(),
                })
            })
        }

        fn abort_turn<'a>(
            &'a self,
            input: GatewayAbortInput<'a>,
        ) -> Pin<Box<dyn Future<Output = Result<(), JsString>> + Send + 'a>> {
            Box::pin(async move {
                let mut entry = serde_json::Map::new();
                entry.insert("op".into(), "abortTurn".into());
                let mut wire = serde_json::Map::new();
                wire.insert("sessionKey".into(), serde_json::json!(input.session_key));
                if let Some(run_id) = input.run_id {
                    wire.insert("runId".into(), serde_json::json!(run_id));
                }
                wire.insert("reason".into(), serde_json::json!(input.reason));
                entry.insert("input".into(), serde_json::Value::Object(wire));
                self.calls
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .push(serde_json::Value::Object(entry));
                Ok(())
            })
        }
    }

    fn plan_fixture(home: &Path) {
        // The storage id of the scratch project — the same derivation the
        // runtime's paths went through, so the fixture lands where the
        // manager's stores read it.
        let project_key = home.join("proj").to_string_lossy().into_owned();
        let metadata = crate::metadata::Metadata {
            home: home.to_path_buf(),
            cwd: PathBuf::new(),
            files: crate::metadata::NativeFiles,
        };
        let paths = super::super::resolve_always_on_paths(home, &project_key, &metadata)
            .expect("#2294 paths");
        std::fs::create_dir_all(
            paths
                .plan_index_file
                .parent()
                .expect("#2294 plan index parent"),
        )
        .expect("#2294 mkdir");
        std::fs::write(
            &paths.plan_index_file,
            serde_json::to_string_pretty(&serde_json::json!({
                "schemaVersion": 1,
                "plans": [{
                    "id": "p1", "title": "T", "createdAt": "2026-01-01T00:00:00.000Z",
                    "status": "ready", "summary": "s", "rationale": "r", "dedupeKey": "d",
                    "sourceRunId": "r0", "planFilePath": "plans/p1.md"
                }]
            }))
            .expect("#2294 fixture json"),
        )
        .expect("#2294 plan index");
    }

    fn cycle_fixture(home: &Path) {
        let project_key = home.join("proj").to_string_lossy().into_owned();
        let metadata = crate::metadata::Metadata {
            home: home.to_path_buf(),
            cwd: PathBuf::new(),
            files: crate::metadata::NativeFiles,
        };
        let paths = super::super::resolve_always_on_paths(home, &project_key, &metadata)
            .expect("#2294 paths");
        std::fs::create_dir_all(
            paths
                .cycle_index_file
                .parent()
                .expect("#2294 cycle index parent"),
        )
        .expect("#2294 mkdir");
        std::fs::write(
            &paths.cycle_index_file,
            serde_json::to_string_pretty(&serde_json::json!({
                "schemaVersion": 1,
                "cycles": [{
                    "id": "c1", "projectKey": project_key, "status": "active", "planIds": [],
                    "createdAt": "2026-01-01T00:00:00.000Z", "createdByRunId": "r0"
                }]
            }))
            .expect("#2294 fixture json"),
        )
        .expect("#2294 cycle index");
    }

    // r1 (generate.mts:509-564): the runtime's control envelopes — not_ready
    // before bind, the two rerun data checks, cycle_not_found, the
    // workspace-less apply deviation, abort ownership, and the not_ready that
    // begins the moment stop does.
    #[tokio::test]
    async fn r1_runtime_control_envelopes() {
        let home = scratch("r1");
        let project_key = home.join("proj").to_string_lossy().into_owned();
        let gateway = Arc::new(RecordingGateway {
            calls: Mutex::new(Vec::new()),
        });
        let (_, uuid) = counter_uuid(7);
        let runtime = Arc::new(
            AlwaysOnRuntime::new(
                config(&project_key),
                &home,
                &project_key,
                None,
                Some(Arc::new(|| NOW_MS)),
                Some(uuid),
                None,
                None,
            )
            .expect("#2294 runtime"),
        );

        // r1.1: before bindAgentGateway every method answers not_ready.
        let before = serde_json::json!({
            "apply": apply_json(
                &runtime
                    .apply_cycle(&ApplyInput {
                        project_key: project_key.clone(),
                        work_cycle_id: "c0".into(),
                        project_name: Some("P".into()),
                    })
                    .await
                    .expect("#2294 apply")
            ),
            "abort": abort_json(
                &runtime
                    .abort_run(&AbortInput {
                        project_key: project_key.clone(),
                        session_key: "always-on/discovery:project=x:run=y".into(),
                        reason: None,
                    })
                    .await
            ),
            "rerun": rerun_json(
                &runtime
                    .rerun_plan(&RerunInput {
                        project_key: project_key.clone(),
                        plan_id: "p0".into(),
                    })
                    .await
                    .expect("#2294 rerun")
            ),
        });
        negative_assert_eq!(
            before,
            golden("r1-control-envelopes")["before"],
            "#2294 r1 before-bind not_ready envelopes"
        );

        runtime
            .bind_agent_gateway(gateway.clone(), Some(Arc::new(|| false)))
            .expect("#2294 bind");

        // r1.2: rerun against a missing plan.
        let rerun_missing = runtime
            .rerun_plan(&RerunInput {
                project_key: project_key.clone(),
                plan_id: "p-missing".into(),
            })
            .await
            .expect("#2294 rerun");
        negative_assert_eq!(
            rerun_json(&rerun_missing),
            golden("r1-control-envelopes")["rerunMissing"],
            "#2294 r1 rerunMissing"
        );

        // r1.3: plan record present, markdown missing.
        plan_fixture(&home);
        let rerun_no_body = runtime
            .rerun_plan(&RerunInput {
                project_key: project_key.clone(),
                plan_id: "p1".into(),
            })
            .await
            .expect("#2294 rerun");
        negative_assert_eq!(
            rerun_json(&rerun_no_body),
            golden("r1-control-envelopes")["rerunNoBody"],
            "#2294 r1 rerunNoBody"
        );

        // r1.4: apply against a missing cycle.
        let apply_missing = runtime
            .apply_cycle(&ApplyInput {
                project_key: project_key.clone(),
                work_cycle_id: "c-missing".into(),
                project_name: Some("P".into()),
            })
            .await
            .expect("#2294 apply");
        negative_assert_eq!(
            apply_json(&apply_missing),
            golden("r1-control-envelopes")["applyMissing"],
            "#2294 r1 applyMissing"
        );

        // r1.5: apply against a workspace-less cycle. The TS runtime path
        // throws the engine TypeError (golden applyNoWorkspace); the port
        // answers the standalone-fallback envelope instead — the one
        // documented deviation of the fire seam
        // (A7.8-ALWAYS-ON-BACKGROUND-001-R1.md).
        cycle_fixture(&home);
        let apply_no_workspace = runtime
            .apply_cycle(&ApplyInput {
                project_key: project_key.clone(),
                work_cycle_id: "c1".into(),
                project_name: Some("P".into()),
            })
            .await
            .expect("#2294 apply answers the envelope, not a TypeError");
        negative_assert_eq!(
            apply_json(&apply_no_workspace),
            serde_json::json!({
                "sessionKey": "",
                "error": {
                    "code": "missing_workspace",
                    "message": "Cycle has no associated workspace to apply"
                }
            }),
            "#2294 r1 applyNoWorkspace: the ported deviation envelope"
        );

        // r1.6: abort for a session no run owns.
        let abort_unknown = runtime
            .abort_run(&AbortInput {
                project_key: project_key.clone(),
                session_key: "always-on/discovery:project=elsewhere:run=z".into(),
                reason: None,
            })
            .await;
        negative_assert_eq!(
            abort_json(&abort_unknown),
            golden("r1-control-envelopes")["abortUnknown"],
            "#2294 r1 abortUnknown"
        );

        // r1.7: abort for a session this runtime's context registry owns.
        let owned_key = "always-on/discovery:project=x:run=owned";
        runtime
            .run_contexts()
            .register(AlwaysOnRunContext {
                session_key: owned_key.into(),
                run_id: "owned-run".into(),
                project_key: project_key.clone(),
            })
            .expect("#2294 register");
        let abort_owned = runtime
            .abort_run(&AbortInput {
                project_key: project_key.clone(),
                session_key: owned_key.into(),
                reason: Some("probe/abort".into()),
            })
            .await;
        negative_assert_eq!(
            abort_json(&abort_owned),
            golden("r1-control-envelopes")["abortOwned"],
            "#2294 r1 abortOwned"
        );

        // r1.8: once stop() begins every method answers not_ready. The spawn
        // plus one yield runs stop's synchronous prefix (`stopping = true`),
        // the probe's `const stopping = runtime.stop()` shape.
        let stopping = {
            let runtime = Arc::clone(&runtime);
            tokio::spawn(async move {
                runtime.stop().await;
            })
        };
        tokio::task::yield_now().await;
        let after_stop_abort = runtime
            .abort_run(&AbortInput {
                project_key: project_key.clone(),
                session_key: owned_key.into(),
                reason: None,
            })
            .await;
        negative_assert_eq!(
            abort_json(&after_stop_abort),
            golden("r1-control-envelopes")["afterStopAbort"],
            "#2294 r1 afterStopAbort"
        );
        stopping.await.expect("#2294 stop completes");
        let calls = gateway
            .calls
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        negative_assert_eq!(
            serde_json::json!({
                "gatewayCalls": calls,
            }),
            serde_json::json!({
                "gatewayCalls": golden("r1-control-envelopes")["gatewayCalls"],
            }),
            "#2294 r1 gatewayCalls: the abort reached the facade once"
        );
    }

    #[test]
    fn native_resolver_rejection_aborts_manager_construction() {
        let home = scratch("resolver-refusal");
        let project_key = home.join("proj").to_string_lossy().into_owned();
        let mut called = 0;
        let result = AlwaysOnManager::new_with_resolver(
            AlwaysOnManagerOptions {
                config: config(&project_key),
                pilot_home: home,
                now: None,
                uuid: None,
                logger: None,
            },
            |key| {
                called += 1;
                assert_eq!(key, project_key);
                Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "marker refused",
                ))
            },
        );
        assert_eq!(called, 1);
        assert!(matches!(result, Err(error) if error.kind() == io::ErrorKind::PermissionDenied));
    }

    #[test]
    fn native_fire_factory_is_used_once_per_enabled_project_before_binding() {
        struct Factory(std::sync::atomic::AtomicUsize);
        impl FireFactory for Factory {
            fn create(&self, input: FireInputs) -> io::Result<Arc<dyn DiscoveryFirePort>> {
                self.0.fetch_add(1, Ordering::SeqCst);
                assert!(input.project_key.ends_with("/proj"));
                Ok(Arc::new(DiscoveryFire::new(input.plan_store)))
            }
        }
        let home = scratch("fire-factory");
        let project_key = home.join("proj").to_string_lossy().into_owned();
        let manager = AlwaysOnManager::new(AlwaysOnManagerOptions {
            config: config(&project_key),
            pilot_home: home,
            now: None,
            uuid: None,
            logger: None,
        })
        .unwrap();
        let factory = Arc::new(Factory(std::sync::atomic::AtomicUsize::new(0)));
        manager.set_fire_factory(factory.clone()).unwrap();
        let gateway = Arc::new(RecordingGateway {
            calls: Mutex::new(Vec::new()),
        });
        manager.bind_agent_gateway(gateway, None).unwrap();
        assert_eq!(factory.0.load(Ordering::SeqCst), 1);
        assert!(manager.set_fire_factory(factory).is_err());
    }

    // r9 (generate.mts:567-588): the manager answers project_not_found for
    // unknown keys — the template-literal message with the raw key.
    #[tokio::test]
    async fn r9_manager_project_not_found() {
        let home = scratch("r9");
        let project_key = home.join("proj").to_string_lossy().into_owned();
        let (_, uuid) = counter_uuid(90);
        let manager = AlwaysOnManager::new(AlwaysOnManagerOptions {
            config: config(&project_key),
            pilot_home: home,
            now: Some(Arc::new(|| NOW_MS)),
            uuid: Some(uuid),
            logger: None,
        })
        .expect("#2294 manager");
        let results = serde_json::json!({
            "apply": apply_json(
                &manager
                    .apply_cycle(&ApplyInput {
                        project_key: "/elsewhere".into(),
                        work_cycle_id: "c0".into(),
                        project_name: Some("P".into()),
                    })
                    .await
                    .expect("#2294 apply")
            ),
            "abort": abort_json(
                &manager
                    .abort_run(&AbortInput {
                        project_key: "/elsewhere".into(),
                        session_key: "k".into(),
                        reason: None,
                    })
                    .await
            ),
            "rerun": rerun_json(
                &manager
                    .rerun_plan(&RerunInput {
                        project_key: "/elsewhere".into(),
                        plan_id: "p0".into(),
                    })
                    .await
                    .expect("#2294 rerun")
            ),
        });
        manager.stop().await.expect("manager stop");
        negative_assert_eq!(
            results,
            golden("r9-manager-project-not-found"),
            "#2294 r9 manager project_not_found"
        );
    }

    // AlwaysOnRuntime.ts:239-241: the double-bind refusal message.
    #[tokio::test]
    async fn double_bind_is_refused() {
        let home = scratch("double-bind");
        let project_key = home.join("proj").to_string_lossy().into_owned();
        let runtime = AlwaysOnRuntime::new(
            config(&project_key),
            &home,
            &project_key,
            None,
            None,
            None,
            None,
            None,
        )
        .expect("#2294 runtime");
        let gateway: Arc<dyn AlwaysOnAgentGateway> = Arc::new(RecordingGateway {
            calls: Mutex::new(Vec::new()),
        });
        runtime
            .bind_agent_gateway(gateway.clone(), None)
            .expect("#2294 first bind");
        let error = runtime
            .bind_agent_gateway(gateway, None)
            .expect_err("#2294 second bind is refused");
        negative_assert_eq!(
            error.to_string(),
            "AlwaysOnRuntime.bindAgentGateway already called.",
            "#2294 double-bind message"
        );
        runtime.stop().await;
    }

    // AlwaysOnRuntime.ts:289-291: start before bind is refused with the TS
    // message.
    #[tokio::test]
    async fn start_before_bind_is_refused() {
        let home = scratch("start-before-bind");
        let project_key = home.join("proj").to_string_lossy().into_owned();
        let runtime = AlwaysOnRuntime::new(
            config(&project_key),
            &home,
            &project_key,
            None,
            None,
            None,
            None,
            None,
        )
        .expect("#2294 runtime");
        let error = runtime
            .start()
            .await
            .expect_err("#2294 start before bind is refused");
        negative_assert_eq!(
            error.to_string(),
            "AlwaysOnRuntime.start called before bindGateway.",
            "#2294 start-before-bind message"
        );
        runtime.stop().await;
    }

    #[tokio::test]
    async fn fire_close_failure_is_reported_on_repeated_stop() {
        struct FailedClose;
        impl DiscoveryFirePort for FailedClose {
            fn close<'a>(&'a self) -> Pin<Box<dyn Future<Output = io::Result<()>> + Send + 'a>> {
                Box::pin(async { Err(io::Error::other("workspace close failed")) })
            }
            fn run<'a>(
                &'a self,
                _: super::super::fire::FireRunInput<'a>,
            ) -> Pin<
                Box<
                    dyn Future<
                            Output = Result<
                                super::super::fire::FireOutcome,
                                super::super::AlwaysOnError,
                            >,
                        > + Send
                        + 'a,
                >,
            > {
                Box::pin(async { unreachable!() })
            }
            fn rerun_plan<'a>(
                &'a self,
                _: super::super::fire::RerunFireInput<'a>,
            ) -> Pin<
                Box<
                    dyn Future<Output = io::Result<super::super::fire::RerunFireResult>>
                        + Send
                        + 'a,
                >,
            > {
                Box::pin(async { unreachable!() })
            }
            fn run_apply_phase<'a>(
                &'a self,
                _: super::super::fire::ApplyPhaseInput<'a>,
            ) -> Pin<
                Box<
                    dyn Future<Output = io::Result<super::super::fire::ApplyPhaseResult>>
                        + Send
                        + 'a,
                >,
            > {
                Box::pin(async { unreachable!() })
            }
        }
        let home = scratch("close-failure");
        let key = home.join("proj").to_string_lossy().into_owned();
        let runtime =
            AlwaysOnRuntime::new(config(&key), &home, &key, None, None, None, None, None).unwrap();
        runtime.swap_fire_for_test(Arc::new(FailedClose));
        assert!(
            runtime
                .stop_checked()
                .await
                .unwrap_err()
                .to_string()
                .contains("workspace close failed")
        );
        let repeated =
            tokio::time::timeout(std::time::Duration::from_secs(1), runtime.stop_checked())
                .await
                .expect("repeat must not hang");
        assert!(
            repeated
                .unwrap_err()
                .to_string()
                .contains("workspace close failed")
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn stop_between_control_precheck_and_registration_rejects_late_run() {
        let home = scratch("stop-before-registration");
        let key = home.join("proj").to_string_lossy().into_owned();
        let (entered_tx, entered) = tokio::sync::oneshot::channel();
        let entered_tx = Mutex::new(Some(entered_tx));
        let release = Arc::new((Mutex::new(false), std::sync::Condvar::new()));
        let parked = release.clone();
        let uuid = Arc::new(move || {
            entered_tx.lock().unwrap().take().unwrap().send(()).unwrap();
            let (lock, changed) = &*parked;
            let (ready, _) = changed
                .wait_timeout_while(
                    lock.lock().unwrap(),
                    std::time::Duration::from_secs(5),
                    |ready| !*ready,
                )
                .unwrap();
            assert!(*ready, "test must release the parked control request");
            "late-run".into()
        });
        let runtime = Arc::new(
            AlwaysOnRuntime::new(
                config(&key),
                &home,
                &key,
                None,
                None,
                Some(uuid),
                None,
                None,
            )
            .unwrap(),
        );
        runtime.swap_fire_for_test(Arc::new(DiscoveryFire::new(runtime.plan_store.clone())));
        let task_runtime = runtime.clone();
        let request = tokio::spawn(async move {
            task_runtime
                .rerun_plan(&RerunInput {
                    project_key: key,
                    plan_id: "missing".into(),
                })
                .await
        });
        tokio::time::timeout(std::time::Duration::from_secs(2), entered)
            .await
            .unwrap()
            .unwrap();
        runtime.stop_checked().await.unwrap();
        *release.0.lock().unwrap() = true;
        release.1.notify_all();
        let result = request.await.unwrap().unwrap();
        assert_eq!(result.error.unwrap().code, "not_ready");
        assert!(
            runtime
                .shared
                .active_control_runs
                .lock()
                .unwrap()
                .is_empty()
        );
        std::fs::remove_dir_all(home).unwrap();
    }

    // #2486: the assembly's synchronous builder polls the automation start
    // chain once, and a failing start's cleanup is dropped mid
    // `AlwaysOnRuntime::stop` (the scheduler's ticker join). TS's
    // `stopPromise` is never dropped mid-flight; the port's drain guard
    // marks the dropped drain abandoned, so the compensating stop re-runs
    // it to completion instead of waiting on a `stop_finished` nobody will
    // ever send.
    #[tokio::test(flavor = "current_thread")]
    async fn a_dropped_stop_is_rerun_by_the_next_stop() {
        use futures_util::FutureExt;
        let home = scratch("dropped-stop");
        let project_key = home.join("proj").to_string_lossy().into_owned();
        let runtime = AlwaysOnRuntime::new(
            config(&project_key),
            &home,
            &project_key,
            None,
            None,
            None,
            None,
            None,
        )
        .expect("#2294 runtime");
        let gateway: Arc<dyn AlwaysOnAgentGateway> = Arc::new(RecordingGateway {
            calls: Mutex::new(Vec::new()),
        });
        runtime
            .bind_agent_gateway(gateway, None)
            .expect("#2294 bind");
        runtime.start().await.expect("#2294 start");
        // One poll of the stop cannot get past the ticker join — the exact
        // yield the builder's `now_or_never` sees.
        negative_assert_eq!(
            std::pin::pin!(runtime.stop()).now_or_never().is_none(),
            true,
            "#2486 the ticker join yields under the single poll"
        );
        // The compensating stop re-runs the abandoned drain — without the
        // guard this await would hang on `stop_finished` forever.
        runtime.stop().await;
        negative_assert_eq!(
            *runtime.shared.stop_finished.subscribe().borrow(),
            true,
            "#2486 the re-run drain settles the stop signal for later waiters"
        );
    }

    // Matrix §三 r3 / AlwaysOnRuntime.ts:296-315: stop waits out an in-flight
    // control run without aborting it. The run is a rerun parked inside its
    // fire await; stop resolves only after the fire releases, and the abort
    // the parked run would answer during the drain is not_ready (stopping).
    #[tokio::test]
    async fn stop_waits_out_the_in_flight_control_run() {
        let home = scratch("stop-waits");
        let project_key = home.join("proj").to_string_lossy().into_owned();
        let release = Arc::new(tokio::sync::Notify::new());
        struct BlockingFire {
            release: Arc<tokio::sync::Notify>,
            entered: Arc<AtomicBool>,
            closed: Arc<AtomicBool>,
        }
        impl DiscoveryFirePort for BlockingFire {
            fn close<'a>(&'a self) -> Pin<Box<dyn Future<Output = io::Result<()>> + Send + 'a>> {
                Box::pin(async move {
                    assert!(
                        !self.entered.load(Ordering::SeqCst),
                        "close follows control-run drain"
                    );
                    self.closed.store(true, Ordering::SeqCst);
                    Ok(())
                })
            }
            fn run<'a>(
                &'a self,
                _input: super::super::fire::FireRunInput<'a>,
            ) -> Pin<
                Box<
                    dyn Future<
                            Output = Result<
                                super::super::fire::FireOutcome,
                                super::super::AlwaysOnError,
                            >,
                        > + Send
                        + 'a,
                >,
            > {
                Box::pin(async { unreachable!("#2294 the control rerun never fires a discovery") })
            }
            fn rerun_plan<'a>(
                &'a self,
                input: super::super::fire::RerunFireInput<'a>,
            ) -> Pin<
                Box<
                    dyn Future<Output = io::Result<super::super::fire::RerunFireResult>>
                        + Send
                        + 'a,
                >,
            > {
                let release = self.release.clone();
                let entered = self.entered.clone();
                Box::pin(async move {
                    entered.store(true, Ordering::SeqCst);
                    release.notified().await;
                    entered.store(false, Ordering::SeqCst);
                    Ok(super::super::fire::RerunFireResult {
                        run_id: input.run_id.into(),
                        error: None,
                    })
                })
            }
            fn run_apply_phase<'a>(
                &'a self,
                _input: super::super::fire::ApplyPhaseInput<'a>,
            ) -> Pin<
                Box<
                    dyn Future<Output = io::Result<super::super::fire::ApplyPhaseResult>>
                        + Send
                        + 'a,
                >,
            > {
                Box::pin(async { unreachable!("#2294 the control rerun never applies") })
            }
        }
        let (_, uuid) = counter_uuid(90);
        let runtime = Arc::new(
            AlwaysOnRuntime::new(
                config(&project_key),
                &home,
                &project_key,
                None,
                Some(Arc::new(|| NOW_MS)),
                Some(uuid),
                None,
                None,
            )
            .expect("#2294 runtime"),
        );
        let gateway: Arc<dyn AlwaysOnAgentGateway> = Arc::new(RecordingGateway {
            calls: Mutex::new(Vec::new()),
        });
        runtime
            .bind_agent_gateway(gateway, None)
            .expect("#2294 bind");
        let entered = Arc::new(AtomicBool::new(false));
        let closed = Arc::new(AtomicBool::new(false));
        runtime.swap_fire_for_test(Arc::new(BlockingFire {
            release: release.clone(),
            entered: entered.clone(),
            closed: closed.clone(),
        }));

        // Park a rerun inside its fire await.
        let parked = {
            let runtime = Arc::clone(&runtime);
            let project_key = project_key.clone();
            tokio::spawn(async move {
                runtime
                    .rerun_plan(&RerunInput {
                        project_key,
                        plan_id: "p1".into(),
                    })
                    .await
            })
        };
        while !entered.load(Ordering::SeqCst) {
            tokio::task::yield_now().await;
        }

        // The parked run's apply key is claimed while the run is in flight.
        let mut claimed = runtime
            .shared
            .active_control_runs
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .keys()
            .cloned()
            .collect::<Vec<_>>();
        claimed.sort();
        negative_assert_eq!(claimed.len(), 1, "#2294 one active control run");
        // The parked run is a rerun, so its claim is the workspace session
        // key (AlwaysOnRuntime.ts:423-429).
        let workspace_key = super::super::derive_workspace_session_key(&project_key, &claimed[0]);
        let during = runtime
            .abort_run(&AbortInput {
                project_key: project_key.clone(),
                session_key: workspace_key,
                reason: None,
            })
            .await;
        // TS abortRun on a control-run-claimed key reaches the gateway (the
        // recording gateway answers Ok).
        negative_assert_eq!(during.aborted, true, "#2294 the control run claims its key");

        // stop begins: not_ready for new entries, and the parked run is
        // waited out — never aborted, its result still delivered.
        let stopping = {
            let runtime = Arc::clone(&runtime);
            tokio::spawn(async move {
                runtime.stop().await;
            })
        };
        tokio::task::yield_now().await;
        let during_stop = runtime
            .rerun_plan(&RerunInput {
                project_key: project_key.clone(),
                plan_id: "p2".into(),
            })
            .await
            .expect("#2294 not_ready is a resolved envelope");
        negative_assert_eq!(
            rerun_json(&during_stop)["error"]["code"],
            "not_ready",
            "#2294 stopping answers not_ready"
        );
        // The stop cannot have finished: the fire has not released.
        negative_assert_eq!(
            stopping.is_finished(),
            false,
            "#2294 stop parks while the control run is in flight"
        );
        release.notify_one();
        let parked_result = parked
            .await
            .expect("#2294 the parked task completes")
            .expect("#2294 the parked rerun resolves");
        negative_assert_eq!(
            parked_result.error.is_none(),
            true,
            "#2294 the in-flight control run was waited out, not aborted"
        );
        stopping.await.expect("#2294 stop completes");
        assert!(
            closed.load(Ordering::SeqCst),
            "fire close completed before stop returned"
        );
    }
}
