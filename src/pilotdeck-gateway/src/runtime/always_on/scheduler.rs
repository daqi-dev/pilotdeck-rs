//! Port of TS `src/always-on/runtime/DiscoveryScheduler.ts` — the periodic
//! tick and its gate chain.
//!
//! A period arriving is not a discovery run: each tick walks
//! gates → cycle-full → lock, and any of the three can block it
//! (DiscoveryScheduler.ts:92-143). The timer side runs as a tokio task:
//! `start` arms the first full interval, `stop` sets the flag and *joins* —
//! an in-flight tick always completes, never aborts (`w1`). A dormancy
//! signal reschedules: the pending interval is dropped and a fresh full one
//! armed (`handleSignal`'s `clearTimeout` + `scheduleNextTick`), which is
//! why the loop yields once on wake before ticking — at an equal deadline
//! the signal must win over the tick it reschedules, the Node timer
//! insertion order the `d1` baseline pinned (its debounce cleared the
//! pending tick timer first).
//!
//! The tick bodies use the same synchronous fs stores as the rest of the
//! family (`state.rs`/`cycles.rs`), so no timer test needs the in-memory
//! substitution the TS probe documented — the Rust runtime runs them inline.
use std::{
    io,
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use pilotdeck_protocol::{JsObject, JsValue};
use tokio::time::sleep;

use crate::runtime::cron::iso_from_epoch_ms;

use super::{
    AlwaysOnConfig, AlwaysOnError, AlwaysOnLogger, AlwaysOnPaths, ChannelLeaseRegistry,
    DEFAULT_MAX_PLANS_PER_CYCLE, DiscoveryStateStore, GateBlockReason, GateInput, SignalWatcher,
    SignalWatcherOptions, WorkCycleStore, acquire_discovery_lock,
    evaluate_always_on_discovery_gates,
    fire::{DiscoveryFirePort, FireRunInput},
    release_discovery_lock,
};

/// The three wire method names the background runtime serves — the
/// capabilities table and the describe probe read this, the same single-owner
/// pattern `CRON_METHODS` follows (#1325).
pub(crate) const ALWAYS_ON_METHODS: &[&str] =
    &["always_on_apply", "always_on_abort", "always_on_rerun_plan"];

/// `DiscoverySchedulerDependencies` (DiscoveryScheduler.ts:24-36). `now` is
/// epoch milliseconds — the Date the TS carries, in the port's clock idiom.
pub(crate) struct SchedulerDeps {
    pub config: AlwaysOnConfig,
    pub project_key: String,
    pub paths: AlwaysOnPaths,
    pub state_store: Arc<DiscoveryStateStore>,
    pub cycle_store: Arc<WorkCycleStore>,
    pub leases: Arc<ChannelLeaseRegistry>,
    pub fire: Arc<dyn DiscoveryFirePort>,
    pub uuid: Arc<dyn Fn() -> String + Send + Sync>,
    pub now: Arc<dyn Fn() -> f64 + Send + Sync>,
    pub logger: Arc<dyn AlwaysOnLogger>,
    pub is_session_in_flight: Arc<dyn Fn() -> bool + Send + Sync>,
}

/// The tick's error: a raw fs failure before the fire try block keeps its own
/// face (the TS tick rejects with the Error), while anything the fire try
/// block threw crosses the catch as an `AlwaysOnError` with the code
/// preserved — `internal` for non-AlwaysOnError throws
/// (DiscoveryScheduler.ts:158-167).
#[derive(Debug)]
pub(crate) enum TickError {
    Io(io::Error),
    Fire(AlwaysOnError),
}
impl TickError {
    pub(crate) fn message(&self) -> String {
        match self {
            Self::Io(error) => error.to_string(),
            Self::Fire(error) => error
                .message()
                .to_utf8()
                .unwrap_or_else(|_| String::from("\u{fffd}")),
        }
    }
}

/// `{ outcome: "fired" | "blocked", reason? }` (DiscoveryScheduler.ts:70).
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct TickOutcome {
    pub outcome: &'static str,
    pub reason: Option<GateBlockReason>,
}

struct SchedulerShared {
    stopped: AtomicBool,
    started: AtomicBool,
    reschedule: AtomicBool,
    notify: tokio::sync::Notify,
    watcher: std::sync::Mutex<Option<SignalWatcher>>,
    task: std::sync::Mutex<Option<tokio::task::JoinHandle<()>>>,
}

pub(crate) struct DiscoveryScheduler {
    deps: SchedulerDeps,
    shared: Arc<SchedulerShared>,
}

impl DiscoveryScheduler {
    pub(crate) fn new(deps: SchedulerDeps) -> Self {
        Self {
            deps,
            shared: Arc::new(SchedulerShared {
                stopped: AtomicBool::new(false),
                started: AtomicBool::new(false),
                reschedule: AtomicBool::new(false),
                notify: tokio::sync::Notify::new(),
                watcher: std::sync::Mutex::new(None),
                task: std::sync::Mutex::new(None),
            }),
        }
    }

    /// `start` (DiscoveryScheduler.ts:47-53): restore any persisted dormancy
    /// watcher, then arm the first full interval.
    pub(crate) async fn start(self: &Arc<Self>) -> io::Result<()> {
        if self.shared.stopped.load(Ordering::SeqCst)
            || self.shared.started.swap(true, Ordering::SeqCst)
        {
            return Ok(());
        }
        self.maybe_restore_dormancy()?;
        let scheduler = Arc::clone(self);
        let task = tokio::spawn(async move {
            scheduler.run_ticker().await;
        });
        *self.shared.task.lock().unwrap_or_else(|e| e.into_inner()) = Some(task);
        Ok(())
    }

    /// `stop` (DiscoveryScheduler.ts:55-67): set the flag, drop the watcher,
    /// wake the loop, and join the ticker — the in-flight tick, if any,
    /// runs to completion first (`w1`).
    pub(crate) async fn stop(&self) {
        self.shared.stopped.store(true, Ordering::SeqCst);
        self.dispose_watcher();
        self.shared.notify.notify_one();
        // The guard is a temporary of the `if let` condition, which would
        // otherwise live across the await below and make this future
        // non-`Send`; the hoisted `let` drops it at the end of the statement.
        let task = self
            .shared
            .task
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take();
        if let Some(task) = task {
            let _ = task.await;
        }
    }

    /// `runTickOnce` (DiscoveryScheduler.ts:70-73), public for tests: after a
    /// stop it answers the `project_disabled` shape.
    pub(crate) async fn run_tick_once(self: &Arc<Self>) -> Result<TickOutcome, TickError> {
        if self.shared.stopped.load(Ordering::SeqCst) {
            return Ok(TickOutcome {
                outcome: "blocked",
                reason: Some(GateBlockReason::ProjectDisabled),
            });
        }
        self.tick().await
    }

    async fn run_ticker(self: Arc<Self>) {
        loop {
            let interval = std::time::Duration::from_millis(self.interval_ms());
            let timer = tokio::select! {
                _ = sleep(interval) => true,
                _ = self.shared.notify.notified() => false,
            };
            if !timer {
                // A notify only ever means stop or reschedule — never a
                // tick. A reschedule whose drop-wake came from the interval
                // timer leaves this permit stale; consuming it here re-arms a
                // fresh full interval instead of firing a tick at the
                // signal's instant, the `clearTimeout` + `scheduleNextTick`
                // shape (the `d1` baseline).
                if self.shared.stopped.load(Ordering::SeqCst) {
                    break;
                }
                self.shared.reschedule.swap(false, Ordering::SeqCst);
                continue;
            }
            // Let a signal landing at the same deadline win over the tick it
            // reschedules (the `d1` baseline's clearTimeout ordering).
            tokio::task::yield_now().await;
            if self.shared.stopped.load(Ordering::SeqCst) {
                break;
            }
            if self.shared.reschedule.swap(false, Ordering::SeqCst) {
                continue;
            }
            if let Err(error) = self.tick().await {
                self.log_warn(
                    "always-on tick failed",
                    &[("error", JsValue::String(error.message().into()))],
                );
            }
        }
    }

    /// `scheduleNextTick`'s interval (DiscoveryScheduler.ts:75-90):
    /// `max(1_000, tickIntervalMinutes * 60_000)`.
    fn interval_ms(&self) -> u64 {
        (self.deps.config.tick_interval_minutes * 60_000.0).max(1000.0) as u64
    }

    /// `tick` (DiscoveryScheduler.ts:92-171): gates → cycle-full → lock →
    /// fire, with the crash path preserving the fire's code and the finally
    /// always releasing the lock.
    async fn tick(self: &Arc<Self>) -> Result<TickOutcome, TickError> {
        let now_ms = (self.deps.now)();
        let state = self.deps.state_store.read(now_ms).map_err(TickError::Io)?;
        let fresh = self.deps.leases.list_fresh(
            &self.deps.project_key,
            self.deps.config.heartbeat_stale_seconds,
            Some(now_ms),
        );
        let evaluation = evaluate_always_on_discovery_gates(&GateInput {
            project_key: &self.deps.project_key,
            config: &self.deps.config,
            state: &state,
            leases: &fresh,
            now_ms,
            project_exists: Path::new(&self.deps.project_key).exists(),
            lock_held: self.deps.paths.discovery_lock_file.exists(),
            session_in_flight: Some((self.deps.is_session_in_flight)()),
        });

        if !evaluation.ok {
            if let Some(reason) = evaluation.reason {
                self.log_info(
                    "always-on gate blocked",
                    &[("reason", JsValue::String(reason.as_str().into()))],
                );
                if reason == GateBlockReason::DormantNoSignal {
                    self.ensure_dormancy_watcher();
                }
            }
            return Ok(TickOutcome {
                outcome: "blocked",
                reason: evaluation.reason,
            });
        }

        // The cycle-full check (DiscoveryScheduler.ts:120-130): the active
        // cycle must exist, be active, and hold fewer than the bound.
        let active_cycle_id = state
            .get("activeWorkCycleId")
            .and_then(JsValue::as_string)
            .and_then(|value| value.to_utf8().ok());
        if let Some(cycle_id) = active_cycle_id {
            let active_cycle = self
                .deps
                .cycle_store
                .get_record(&cycle_id)
                .map_err(TickError::Io)?;
            if active_cycle.is_some_and(|cycle| {
                cycle.status == "active" && cycle.plan_ids.len() >= DEFAULT_MAX_PLANS_PER_CYCLE
            }) {
                self.log_info(
                    "always-on gate blocked",
                    &[("reason", JsValue::String("cycle_full".into()))],
                );
                return Ok(TickOutcome {
                    outcome: "blocked",
                    reason: Some(GateBlockReason::CycleFull),
                });
            }
        }

        let run_id = (self.deps.uuid)();
        let started_at_ms = (self.deps.now)();
        let acquired = acquire_discovery_lock(
            &self.deps.paths,
            std::process::id(),
            &run_id,
            &iso_from_epoch_ms(started_at_ms),
        )
        .map_err(TickError::Io)?;
        if !acquired {
            self.log_info(
                "always-on lock_busy",
                &[("runId", JsValue::String(run_id.clone().into()))],
            );
            return Ok(TickOutcome {
                outcome: "blocked",
                reason: Some(GateBlockReason::LockBusy),
            });
        }

        // The fire try block (DiscoveryScheduler.ts:145-167): the three
        // pre-fire writes, the run, and the crash path that preserves the
        // AlwaysOnError code — an io failure in here wraps as `internal`.
        let fired = async {
            self.deps
                .state_store
                .mark_fire_started(&run_id, started_at_ms)
                .map_err(TickError::Io)?;
            self.deps
                .state_store
                .clear_dormant(started_at_ms)
                .map_err(TickError::Io)?;
            self.dispose_watcher();
            let result = self
                .deps
                .fire
                .run(FireRunInput {
                    run_id: &run_id,
                    started_at_ms,
                })
                .await
                .map_err(TickError::Fire)?;
            Ok::<_, TickError>(result)
        }
        .await;
        match fired {
            Ok(result) => {
                self.log_info(
                    "always-on fire complete",
                    &[
                        ("runId", JsValue::String(run_id.clone().into())),
                        ("outcome", JsValue::String(result.outcome.clone().into())),
                    ],
                );
                if result.outcome == "no_plan" {
                    self.ensure_dormancy_watcher();
                }
                release_discovery_lock(&self.deps.paths);
                Ok(TickOutcome {
                    outcome: "fired",
                    reason: None,
                })
            }
            Err(error) => {
                let message = error.message();
                self.log_warn(
                    "always-on fire crashed",
                    &[
                        ("runId", JsValue::String(run_id.clone().into())),
                        ("error", JsValue::String(message.clone().into())),
                    ],
                );
                release_discovery_lock(&self.deps.paths);
                Err(match error {
                    TickError::Fire(fire) => TickError::Fire(fire),
                    TickError::Io(_) => TickError::Fire(AlwaysOnError::new("internal", message)),
                })
            }
        }
    }

    /// `ensureDormancyWatcher` (DiscoveryScheduler.ts:173-190).
    fn ensure_dormancy_watcher(self: &Arc<Self>) {
        if self
            .shared
            .watcher
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .is_some()
        {
            return;
        }
        let scheduler = Arc::downgrade(self);
        let on_signal: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
            if let Some(scheduler) = scheduler.upgrade() {
                scheduler.handle_signal();
            }
        });
        let scheduler = Arc::downgrade(self);
        let on_error: Arc<dyn Fn(&str) + Send + Sync> = Arc::new(move |message: &str| {
            if let Some(scheduler) = scheduler.upgrade() {
                scheduler.log_warn(
                    "always-on signal watcher error",
                    &[("error", JsValue::String(message.into()))],
                );
                scheduler.dispose_watcher();
            }
        });
        let watcher = SignalWatcher::new(SignalWatcherOptions::with_defaults(
            Path::new(&self.deps.project_key).to_path_buf(),
            on_signal,
            on_error,
        ));
        watcher.start();
        *self
            .shared
            .watcher
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = Some(watcher);
    }

    /// `handleSignal` (DiscoveryScheduler.ts:192-201): clear the dormancy,
    /// dispose the watcher, drop the pending interval and arm a fresh full
    /// one. A clearDormant fs failure is swallowed the way the TS `void`
    /// leaves it an unhandled rejection — the reschedule still happens.
    fn handle_signal(&self) {
        if self.shared.stopped.load(Ordering::SeqCst) {
            return;
        }
        let _ = self.deps.state_store.clear_dormant((self.deps.now)());
        self.dispose_watcher();
        self.shared.reschedule.store(true, Ordering::SeqCst);
        self.shared.notify.notify_one();
    }

    /// `disposeWatcher` (DiscoveryScheduler.ts:203-206).
    fn dispose_watcher(&self) {
        if let Some(watcher) = self
            .shared
            .watcher
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take()
        {
            watcher.stop();
        }
    }

    /// `maybeRestoreDormancy` (DiscoveryScheduler.ts:208-213).
    fn maybe_restore_dormancy(self: &Arc<Self>) -> io::Result<()> {
        let state = self.deps.state_store.read((self.deps.now)())?;
        if !matches!(
            state.get("dormant"),
            None | Some(JsValue::Undefined) | Some(JsValue::Null)
        ) {
            self.ensure_dormancy_watcher();
        }
        Ok(())
    }

    fn log_info(&self, message: &str, meta: &[(&'static str, JsValue)]) {
        self.deps.logger.info(message, &log_meta(meta));
    }

    fn log_warn(&self, message: &str, meta: &[(&'static str, JsValue)]) {
        self.deps.logger.warn(message, &log_meta(meta));
    }
}

/// The metadata object exactly as the TS call sites build it (an ordered
/// literal); `JSON.stringify` of it is what the golden logs pinned.
fn log_meta(pairs: &[(&'static str, JsValue)]) -> JsObject {
    let mut object = JsObject::new();
    for (key, value) in pairs {
        object.insert((*key).into(), value.clone());
    }
    object
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::negative_assert_eq;
    use std::future::Future;
    use std::io;
    use std::pin::Pin;
    use std::sync::Mutex;

    use super::super::fire::{
        ApplyPhaseInput, ApplyPhaseResult, FireOutcome, RerunFireInput, RerunFireResult,
    };
    use super::super::leases::LeaseUpdate;
    use pilotdeck_protocol::stringify_json;

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

    fn golden_log(id: &str) -> Vec<String> {
        golden(id)["log"]
            .as_array()
            .expect("#2294 golden log")
            .iter()
            .map(|line| line.as_str().expect("#2294 log line").into())
            .collect()
    }

    /// The probe's i1/i2/w1 ran on the in-memory lock pair whose
    /// `lock.acquire`/`lock.release` log lines are a harness artifact of that
    /// substitution; the port runs the real `wx` lock and observes it through
    /// file existence, so those lines are filtered from the golden side.
    fn without_lock_lines(log: Vec<String>) -> Vec<String> {
        log.into_iter()
            .filter(|line| !line.starts_with("lock."))
            .collect()
    }

    fn tick_json(outcome: &TickOutcome) -> serde_json::Value {
        let mut json = serde_json::Map::new();
        json.insert("outcome".into(), serde_json::json!(outcome.outcome));
        if let Some(reason) = outcome.reason {
            json.insert("reason".into(), serde_json::json!(reason.as_str()));
        }
        serde_json::Value::Object(json)
    }

    fn error_json(error: &TickError) -> serde_json::Value {
        match error {
            TickError::Fire(fire) => serde_json::json!({
                "name": "AlwaysOnError",
                "code": fire.code(),
                "message": fire.message().to_utf8().expect("#2294 UTF-8"),
            }),
            TickError::Io(error) => serde_json::json!({
                "name": "Error",
                "message": error.to_string(),
            }),
        }
    }

    /// The probe's `fakeFire` (generate.mts:251-261): records the call, logs
    /// `fire.run:<runId>`, then behaves.
    enum FakeBehavior {
        Ok,
        Crash(AlwaysOnError),
        Block(Arc<tokio::sync::Notify>),
    }
    struct FakeFire {
        calls: Mutex<Vec<String>>,
        log: Arc<Mutex<Vec<String>>>,
        behavior: FakeBehavior,
    }
    impl DiscoveryFirePort for FakeFire {
        fn run<'a>(
            &'a self,
            input: FireRunInput<'a>,
        ) -> Pin<Box<dyn Future<Output = Result<FireOutcome, AlwaysOnError>> + Send + 'a>> {
            Box::pin(async move {
                self.calls
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .push(input.run_id.into());
                self.log
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .push(format!("fire.run:{}", input.run_id));
                match &self.behavior {
                    FakeBehavior::Ok => Ok(FireOutcome {
                        outcome: "executed".into(),
                    }),
                    FakeBehavior::Crash(error) => {
                        Err(AlwaysOnError::new(error.code(), error.message()))
                    }
                    FakeBehavior::Block(release) => {
                        release.notified().await;
                        Ok(FireOutcome {
                            outcome: "executed".into(),
                        })
                    }
                }
            })
        }
        fn rerun_plan<'a>(
            &'a self,
            _input: RerunFireInput<'a>,
        ) -> Pin<Box<dyn Future<Output = io::Result<RerunFireResult>> + Send + 'a>> {
            Box::pin(async { unreachable!("#2294 the scheduler never reruns") })
        }
        fn run_apply_phase<'a>(
            &'a self,
            _input: ApplyPhaseInput<'a>,
        ) -> Pin<Box<dyn Future<Output = io::Result<ApplyPhaseResult>> + Send + 'a>> {
            Box::pin(async { unreachable!("#2294 the scheduler never applies") })
        }
    }

    /// The probe's `recordingLogger` (generate.mts:157-162):
    /// `level:message:JSON`.
    struct RecordingLogger {
        log: Arc<Mutex<Vec<String>>>,
    }
    impl AlwaysOnLogger for RecordingLogger {
        fn info(&self, message: &str, meta: &JsObject) {
            let meta = stringify_json(&JsValue::Object(meta.clone()))
                .expect("#2294 meta serializes")
                .expect("#2294 meta is defined");
            self.log
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(format!("info:{message}:{meta}"));
        }
        fn warn(&self, message: &str, meta: &JsObject) {
            let meta = stringify_json(&JsValue::Object(meta.clone()))
                .expect("#2294 meta serializes")
                .expect("#2294 meta is defined");
            self.log
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(format!("warn:{message}:{meta}"));
        }
    }

    /// One `tickScenario` (generate.mts:270-299): scratch home, one served
    /// project, the real stores, the fake fire, the recording logger, and a
    /// uuid counter positioned where the probe's global one stood for the
    /// scenario.
    struct Harness {
        project_root: std::path::PathBuf,
        project_key: String,
        paths: AlwaysOnPaths,
        state_store: Arc<DiscoveryStateStore>,
        cycle_store: Arc<WorkCycleStore>,
        leases: Arc<ChannelLeaseRegistry>,
        fire: Arc<FakeFire>,
        log: Arc<Mutex<Vec<String>>>,
        uuid: Arc<Mutex<u32>>,
    }
    impl Harness {
        fn new(tag: &str, first_uuid: u32) -> Self {
            Self::with_behavior(tag, first_uuid, FakeBehavior::Ok)
        }

        fn with_behavior(tag: &str, first_uuid: u32, behavior: FakeBehavior) -> Self {
            let home = std::env::temp_dir().join(format!("always-on-2294-sched-{tag}"));
            let _ = std::fs::remove_dir_all(&home);
            let project_root = home.join("proj");
            std::fs::create_dir_all(&project_root).expect("#2294 project mkdir");
            let project_dir = home.join("always-on").join("projects").join("x");
            let paths = AlwaysOnPaths {
                project_key: project_root.to_string_lossy().into_owned(),
                state_file: project_dir.join("state.json"),
                plans_dir: project_dir.join("plans"),
                plan_index_file: project_dir.join("plans").join("index.json"),
                cycle_index_file: project_dir.join("cycles").join("index.json"),
                reports_dir: project_dir.join("reports"),
                discovery_lock_file: project_dir.join("locks").join("discovery.lock"),
            };
            let log = Arc::new(Mutex::new(Vec::new()));
            Self {
                fire: Arc::new(FakeFire {
                    calls: Mutex::new(Vec::new()),
                    log: log.clone(),
                    behavior,
                }),
                log,
                uuid: Arc::new(Mutex::new(first_uuid - 1)),
                leases: Arc::new(ChannelLeaseRegistry::new(Arc::new(|| NOW_MS))),
                state_store: Arc::new(DiscoveryStateStore::new(paths.clone())),
                cycle_store: Arc::new(WorkCycleStore::new(paths.clone())),
                paths,
                project_key: project_root.to_string_lossy().into_owned(),
                project_root,
            }
        }

        fn config(&self, enabled: bool, trigger: &[(&str, f64)]) -> AlwaysOnConfig {
            let mut config = AlwaysOnConfig::default_config();
            config.projects = vec![(self.project_key.clone(), enabled)];
            for (key, value) in trigger {
                match *key {
                    "tickIntervalMinutes" => config.tick_interval_minutes = *value,
                    "dailyBudget" => config.daily_budget = *value,
                    "cooldownMinutes" => config.cooldown_minutes = *value,
                    _ => unreachable!("#2294 trigger key {key}"),
                }
            }
            config
        }

        fn scheduler(&self, config: AlwaysOnConfig) -> Arc<DiscoveryScheduler> {
            self.scheduler_for(&self.project_key.clone(), config, false)
        }

        /// `scheduler_for` covers the `pre` hook's reach (generate.mts:262-269):
        /// a scenario may point the scheduler at a different project key or
        /// flip the session-in-flight hook.
        fn scheduler_for(
            &self,
            project_key: &str,
            config: AlwaysOnConfig,
            in_flight: bool,
        ) -> Arc<DiscoveryScheduler> {
            let uuid = Arc::new({
                let counter = self.uuid.clone();
                move || {
                    let mut counter = counter.lock().unwrap_or_else(|e| e.into_inner());
                    *counter += 1;
                    format!("probe-run-{counter}")
                }
            });
            Arc::new(DiscoveryScheduler::new(SchedulerDeps {
                config,
                project_key: project_key.into(),
                paths: self.paths.clone(),
                state_store: self.state_store.clone(),
                cycle_store: self.cycle_store.clone(),
                leases: self.leases.clone(),
                fire: self.fire.clone(),
                uuid,
                now: Arc::new(|| NOW_MS),
                logger: Arc::new(RecordingLogger {
                    log: self.log.clone(),
                }),
                is_session_in_flight: Arc::new(move || in_flight),
            }))
        }

        fn write_json(&self, file: &Path, value: &serde_json::Value) {
            if let Some(parent) = file.parent() {
                std::fs::create_dir_all(parent).expect("#2294 mkdir");
            }
            std::fs::write(
                file,
                serde_json::to_string_pretty(value).expect("#2294 fixture json"),
            )
            .expect("#2294 write");
        }

        fn read_json(&self, file: &Path) -> serde_json::Value {
            serde_json::from_str(&std::fs::read_to_string(file).expect("#2294 read"))
                .expect("#2294 parse")
        }

        fn fire_calls(&self) -> Vec<String> {
            self.fire
                .calls
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone()
        }

        fn log(&self) -> Vec<String> {
            self.log.lock().unwrap_or_else(|e| e.into_inner()).clone()
        }

        fn push_log(&self, line: &str) {
            self.log
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(line.into());
        }
    }

    // One blocked-gate scenario (generate.mts:270-299): the tick's answer, the
    // fire count, the lock residue and the log, against the golden.
    async fn blocked_scenario(
        id: &str,
        config: AlwaysOnConfig,
        state: Option<serde_json::Value>,
        lock: Option<serde_json::Value>,
        lease: bool,
        in_flight: bool,
    ) {
        let harness = Harness::new(id, 90);
        if let Some(state) = &state {
            harness.write_json(&harness.paths.state_file, state);
        }
        if let Some(lock) = &lock {
            harness.write_json(&harness.paths.discovery_lock_file, lock);
        }
        if lease {
            harness.leases.set(LeaseUpdate {
                channel_key: "web".into(),
                writer_id: "w1".into(),
                project_key: harness.project_key.clone(),
                session_key: "s".into(),
                agent_busy: false,
                last_user_msg_at: Some("2026-01-01T00:04:30.000Z".into()),
                written_at: None,
            });
        }
        let scheduler = harness.scheduler_for(&harness.project_key.clone(), config, in_flight);
        let result = scheduler.run_tick_once().await.expect("#2294 tick");
        let record = serde_json::json!({
            "result": tick_json(&result),
            "fireCalls": harness.fire_calls().len(),
            "lockAfter": harness.paths.discovery_lock_file.exists(),
            "log": harness.log(),
        });
        negative_assert_eq!(
            record,
            golden(id),
            "#2294 {id}: the blocked tick matches the TS baseline"
        );
        scheduler.stop().await;
    }

    // t2 (generate.mts:303): the project listed disabled in the config.
    #[tokio::test]
    async fn t2_disabled_blocks() {
        let harness = Harness::new("t2-disabled", 90);
        blocked_scenario(
            "t2-disabled",
            harness.config(false, &[]),
            None,
            None,
            false,
            false,
        )
        .await;
    }

    // t3 (generate.mts:304): the scheduler's key is a gone path the config
    // still lists as enabled.
    #[tokio::test]
    async fn t3_missing_project_blocks() {
        let harness = Harness::new("t3-missing", 90);
        let gone = harness.project_root.parent().unwrap().join("gone");
        let mut config = AlwaysOnConfig::default_config();
        config.projects = vec![(gone.to_string_lossy().into_owned(), true)];
        let scheduler = harness.scheduler_for(&gone.to_string_lossy(), config, false);
        let result = scheduler.run_tick_once().await.expect("#2294 tick");
        let record = serde_json::json!({
            "result": tick_json(&result),
            "fireCalls": harness.fire_calls().len(),
            "lockAfter": harness.paths.discovery_lock_file.exists(),
            "log": harness.log(),
        });
        negative_assert_eq!(
            record,
            golden("t3-missing"),
            "#2294 t3-missing: a project that does not exist blocks"
        );
        scheduler.stop().await;
    }

    // t4 (generate.mts:305): the persisted dormancy blocks the tick.
    #[tokio::test]
    async fn t4_dormant_blocks() {
        blocked_scenario(
            "t4-dormant",
            Harness::new("t4-dormant", 90).config(true, &[]),
            Some(serde_json::json!({
                "schemaVersion": 1, "todayKey": "2026-01-01", "todayRunCount": 0,
                "consecutiveFailures": 0,
                "dormant": {
                    "since": "2026-01-01T00:00:00.000Z",
                    "lastBaselineAt": "2026-01-01T00:00:00.000Z"
                }
            })),
            None,
            false,
            false,
        )
        .await;
    }

    // t5 (generate.mts:306): a session in flight is the agent-busy gate.
    #[tokio::test]
    async fn t5_agent_busy_blocks() {
        let harness = Harness::new("t5-agent-busy", 90);
        blocked_scenario(
            "t5-agent-busy",
            harness.config(true, &[]),
            None,
            None,
            false,
            true,
        )
        .await;
    }

    // t6 (generate.mts:307): a completion inside the default cooldown window.
    #[tokio::test]
    async fn t6_cooldown_blocks() {
        blocked_scenario(
            "t6-cooldown",
            Harness::new("t6-cooldown", 90).config(true, &[]),
            Some(serde_json::json!({
                "schemaVersion": 1, "todayKey": "2026-01-01", "todayRunCount": 0,
                "consecutiveFailures": 0, "lastFireCompletedAt": "2026-01-01T00:04:00.000Z"
            })),
            None,
            false,
            false,
        )
        .await;
    }

    // t7 (generate.mts:308): today's runs at the default budget.
    #[tokio::test]
    async fn t7_daily_budget_blocks() {
        blocked_scenario(
            "t7-daily-budget",
            Harness::new("t7-daily-budget", 90).config(true, &[]),
            Some(serde_json::json!({
                "schemaVersion": 1, "todayKey": "2026-01-01", "todayRunCount": 4,
                "consecutiveFailures": 0
            })),
            None,
            false,
            false,
        )
        .await;
    }

    // t8 (generate.mts:309): a pre-existing lock file is the gate's lock_held.
    #[tokio::test]
    async fn t8_lock_busy_blocks() {
        blocked_scenario(
            "t8-lock-busy",
            Harness::new("t8-lock-busy", 90).config(true, &[]),
            None,
            Some(serde_json::json!({"pid": 1, "runId": "held", "startedAt": "x"})),
            false,
            false,
        )
        .await;
    }

    // t9 (generate.mts:310): a fresh lease carrying a recent user message.
    #[tokio::test]
    async fn t9_recent_user_msg_blocks() {
        blocked_scenario(
            "t9-recent-user-msg",
            Harness::new("t9-recent-user-msg", 90).config(true, &[]),
            None,
            None,
            true,
            false,
        )
        .await;
    }

    // t1 (generate.mts:312-330): a clean project fires, with the state writes,
    // the lock residue and the log byte-identical to the baseline.
    #[tokio::test]
    async fn t1_fires_with_state_and_lock_bookkeeping() {
        let harness = Harness::new("t1", 1);
        let scheduler = harness.scheduler(harness.config(true, &[]));
        let result = scheduler.run_tick_once().await.expect("#2294 tick");
        let record = serde_json::json!({
            "result": tick_json(&result),
            "fireCalls": harness.fire_calls(),
            "state": harness.read_json(&harness.paths.state_file),
            "lockAfter": harness.paths.discovery_lock_file.exists(),
            "log": harness.log(),
        });
        negative_assert_eq!(
            record,
            golden("t1-fired"),
            "#2294 t1-fired: the full fire path matches the TS baseline"
        );
        scheduler.stop().await;
    }

    // t10 (generate.mts:331-355): the active cycle at the plan bound blocks
    // before the lock; an under-full cycle proceeds.
    #[tokio::test]
    async fn t10_cycle_full_blocks_before_the_lock() {
        let harness = Harness::new("t10", 2);
        let cycle_index = |plan_ids: serde_json::Value| {
            serde_json::json!({
                "schemaVersion": 1,
                "cycles": [{
                    "id": "c1",
                    "projectKey": harness.project_key,
                    "status": "active",
                    "workspace": {"strategy": "snapshot-copy", "cwd": "/w", "metadata": {}},
                    "planIds": plan_ids,
                    "createdAt": "2026-01-01T00:00:00.000Z",
                    "createdByRunId": "r0",
                }],
            })
        };
        harness.write_json(
            &harness.paths.cycle_index_file,
            &cycle_index(serde_json::json!(["p1", "p2", "p3"])),
        );
        harness.write_json(
            &harness.paths.state_file,
            &serde_json::json!({
                "schemaVersion": 1, "todayKey": "2026-01-01", "todayRunCount": 0,
                "consecutiveFailures": 0, "activeWorkCycleId": "c1"
            }),
        );
        let scheduler = harness.scheduler(harness.config(true, &[]));
        let full = scheduler.run_tick_once().await.expect("#2294 full tick");
        harness.write_json(
            &harness.paths.cycle_index_file,
            &cycle_index(serde_json::json!(["p1", "p2"])),
        );
        let not_full = scheduler
            .run_tick_once()
            .await
            .expect("#2294 under-full tick");
        let record = serde_json::json!({
            "full": tick_json(&full),
            "notFull": tick_json(&not_full),
            "fireCalls": harness.fire_calls().len(),
            "log": harness.log(),
        });
        negative_assert_eq!(
            record,
            golden("t10-cycle-full"),
            "#2294 t10-cycle-full: the cycle bound matches the TS baseline"
        );
        scheduler.stop().await;
    }

    // t11 (generate.mts:356-378): a fire crash rethrows with the code preserved,
    // counts against the daily budget, and still releases the lock.
    #[tokio::test]
    async fn t11_fire_crash_preserves_the_code() {
        let harness = Harness::with_behavior(
            "t11",
            3,
            FakeBehavior::Crash(AlwaysOnError::new(
                "workspace_prepare_failed",
                "probe: fire crashed",
            )),
        );
        let scheduler = harness.scheduler(harness.config(true, &[("dailyBudget", 1.0)]));
        let crash = match scheduler.run_tick_once().await {
            Err(error) => error_json(&error),
            Ok(_) => panic!("#2294 the crash scenario must reject"),
        };
        let next_tick = scheduler.run_tick_once().await.expect("#2294 next tick");
        let record = serde_json::json!({
            "crash": crash,
            "state": harness.read_json(&harness.paths.state_file),
            "lockAfter": harness.paths.discovery_lock_file.exists(),
            "nextTick": tick_json(&next_tick),
            "log": harness.log(),
        });
        negative_assert_eq!(
            record,
            golden("t11-fire-crash"),
            "#2294 t11-fire-crash: the crash path matches the TS baseline"
        );
        scheduler.stop().await;
    }

    // t12 (generate.mts:379-397): runTickOnce after stop answers the
    // project_disabled shape.
    #[tokio::test]
    async fn t12_after_stop_answers_project_disabled() {
        let harness = Harness::new("t12", 90);
        let scheduler = harness.scheduler(harness.config(true, &[]));
        scheduler.start().await.expect("#2294 start");
        scheduler.stop().await;
        let result = scheduler.run_tick_once().await.expect("#2294 tick");
        negative_assert_eq!(
            tick_json(&result),
            golden("t12-after-stop"),
            "#2294 t12-after-stop: the stopped tick's shape"
        );
    }

    // i1 (generate.mts:472, intervalScenario 0 minutes): the interval floors at
    // 1000ms — 999ms elapses without a fire, the floored boundary fires.
    #[tokio::test(start_paused = true)]
    async fn i1_the_interval_floors_at_one_second() {
        let harness = Harness::new("i1-floor-1000ms", 4);
        let scheduler = harness.scheduler(harness.config(true, &[("tickIntervalMinutes", 0.0)]));
        scheduler.start().await.expect("#2294 start");
        tokio::time::sleep(std::time::Duration::from_millis(999)).await;
        let before = harness.fire_calls().len();
        // 51ms past the floored boundary, not the boundary itself: at an
        // equal deadline the test task can resume before the ticker's tick
        // completes.
        tokio::time::sleep(std::time::Duration::from_millis(51)).await;
        let after = harness.fire_calls().len();
        negative_assert_eq!(
            serde_json::json!({"before": before, "after": after}),
            {
                let golden_value = golden("i1-floor-1000ms");
                serde_json::json!({
                    "before": golden_value["before"],
                    "after": golden_value["after"],
                })
            },
            "#2294 i1-floor-1000ms: the floored interval fires once"
        );
        negative_assert_eq!(
            harness.log(),
            without_lock_lines(golden_log("i1-floor-1000ms")),
            "#2294 i1-floor-1000ms: the fired tick's log"
        );
        scheduler.stop().await;
    }

    // i2 (generate.mts:473, intervalScenario 2 minutes): the honest interval —
    // 119_999ms elapses without a fire, the two-minute boundary fires.
    #[tokio::test(start_paused = true)]
    async fn i2_two_minutes_of_interval() {
        let harness = Harness::new("i2-two-minutes", 5);
        let scheduler = harness.scheduler(harness.config(true, &[("tickIntervalMinutes", 2.0)]));
        scheduler.start().await.expect("#2294 start");
        tokio::time::sleep(std::time::Duration::from_millis(119_999)).await;
        let before = harness.fire_calls().len();
        tokio::time::sleep(std::time::Duration::from_millis(1_001)).await;
        let after = harness.fire_calls().len();
        negative_assert_eq!(
            serde_json::json!({"before": before, "after": after}),
            {
                let golden_value = golden("i2-two-minutes");
                serde_json::json!({
                    "before": golden_value["before"],
                    "after": golden_value["after"],
                })
            },
            "#2294 i2-two-minutes: the two-minute interval fires once"
        );
        negative_assert_eq!(
            harness.log(),
            without_lock_lines(golden_log("i2-two-minutes")),
            "#2294 i2-two-minutes: the fired tick's log"
        );
        scheduler.stop().await;
    }

    // The matrix's double-start cell (DiscoveryScheduler.ts:47-53): the
    // running flag makes a second start a no-op — the floored interval
    // still fires exactly once, not once per start call.
    #[tokio::test(start_paused = true)]
    async fn a_second_start_does_not_rearm_the_ticker() {
        let harness = Harness::new("double-start", 6);
        let scheduler = harness.scheduler(harness.config(true, &[("tickIntervalMinutes", 0.0)]));
        scheduler.start().await.expect("#2294 start");
        scheduler.start().await.expect("#2294 start");
        tokio::time::sleep(std::time::Duration::from_millis(999)).await;
        let before = harness.fire_calls().len();
        tokio::time::sleep(std::time::Duration::from_millis(51)).await;
        let after = harness.fire_calls().len();
        negative_assert_eq!(
            before,
            0,
            "#2294 double-start: the floored boundary has not fired yet"
        );
        negative_assert_eq!(
            after,
            1,
            "#2294 double-start: the second start rearms nothing (one fire, not two)"
        );
        scheduler.stop().await;
    }

    // w1 (generate.mts:476-504): stop waits for the in-flight tick, never
    // aborts it — the fire completes, the lock releases, and only then does
    // the stop resolve.
    #[tokio::test(start_paused = true)]
    async fn w1_stop_waits_for_the_in_flight_tick() {
        let release = Arc::new(tokio::sync::Notify::new());
        let harness = Harness::with_behavior("w1", 6, FakeBehavior::Block(release.clone()));
        let scheduler = harness.scheduler(harness.config(true, &[("tickIntervalMinutes", 0.0)]));
        scheduler.start().await.expect("#2294 start");
        // Past the boundary so the tick is parked inside its fire await.
        tokio::time::sleep(std::time::Duration::from_millis(1_051)).await;
        let stop_task = {
            let scheduler = Arc::clone(&scheduler);
            tokio::spawn(async move {
                scheduler.stop().await;
            })
        };
        tokio::time::sleep(std::time::Duration::from_millis(1)).await;
        let during_stop = serde_json::json!({
            "stopDone": stop_task.is_finished(),
            "fireStarted": harness.fire_calls().len() == 1,
            "stateWritten": harness.read_json(&harness.paths.state_file)["lastRunId"],
            "lockHeld": harness.paths.discovery_lock_file.exists(),
        });
        negative_assert_eq!(
            during_stop,
            golden("w1-stop-waits")["duringStop"],
            "#2294 w1-stop-waits: stop parks while the fire runs"
        );
        release.notify_one();
        stop_task.await.expect("#2294 stop completes");
        // The probe's harness line for the resolved stop (generate.mts:491).
        harness.push_log("stop.done");
        let golden_value = golden("w1-stop-waits")["afterStop"].clone();
        let golden_log_lines: Vec<String> = golden_value["log"]
            .as_array()
            .expect("#2294 golden log")
            .iter()
            .map(|line| line.as_str().expect("#2294 log line").into())
            .collect();
        negative_assert_eq!(
            serde_json::json!({
                "stopDone": true,
                "lockAfter": harness.paths.discovery_lock_file.exists(),
                "log": harness.log(),
            }),
            serde_json::json!({
                "stopDone": golden_value["stopDone"],
                "lockAfter": golden_value["lockAfter"],
                "log": without_lock_lines(golden_log_lines),
            }),
            "#2294 w1-stop-waits: the fire completes before the stop resolves"
        );
    }

    // d1 (generate.mts:594-623): a dormant project's ticks all block; a real
    // change under the project root debounces, clears the dormancy, drops the
    // pending tick and arms a fresh full interval, and the next two ticks
    // fire. The equal-deadline ordering (the debounce against the tick it
    // reschedules) is the port's `yield_now` port of the TS `clearTimeout`.
    #[tokio::test(start_paused = true)]
    async fn d1_a_dormancy_signal_clears_and_reschedules() {
        let harness = Harness::new("d1", 11);
        harness.write_json(
            &harness.paths.state_file,
            &serde_json::json!({
                "schemaVersion": 1, "todayKey": "2026-01-01", "todayRunCount": 0,
                "consecutiveFailures": 0,
                "dormant": {
                    "since": "2026-01-01T00:00:00.000Z",
                    "lastBaselineAt": "2026-01-01T00:00:00.000Z"
                }
            }),
        );
        let scheduler = harness.scheduler(harness.config(true, &[("tickIntervalMinutes", 0.0)]));
        // start() restores the dormancy watcher (its baseline at t=0).
        scheduler.start().await.expect("#2294 start");
        tokio::time::sleep(std::time::Duration::from_millis(1_600)).await;
        let fired_while_dormant = harness.fire_calls().len();
        let blocked = harness.log().iter().any(|line| {
            line.contains("always-on gate blocked") && line.contains("dormant_no_signal")
        });
        std::fs::write(harness.project_root.join("signal.txt"), "change").expect("#2294 signal");
        // The poll (1s) sees it, the debounce (2s) settles, the signal clears
        // the dormancy and drops the pending tick, and the fresh interval's
        // next two ticks fire.
        tokio::time::sleep(std::time::Duration::from_millis(4_500)).await;
        let dormant_after = harness
            .read_json(&harness.paths.state_file)
            .get("dormant")
            .cloned()
            .unwrap_or(serde_json::Value::Null);
        let record = serde_json::json!({
            "firedWhileDormant": fired_while_dormant,
            "blocked": blocked,
            "dormantAfter": dormant_after,
            "firedAfterSignal": harness.fire_calls().len(),
            "log": harness.log(),
        });
        negative_assert_eq!(
            record,
            golden("d1-dormant-signal"),
            "#2294 d1-dormant-signal: the dormancy signal semantics"
        );
        scheduler.stop().await;
    }

    /// #2294: the committed oracle must be what the TS generator produces.
    #[test]
    #[ignore = "requires fixed PDRS_BASELINE and Node v22.22.0"]
    fn always_on_oracle_regenerates_identically() {
        crate::test_baseline::regenerates_identically("always-on-2294");
    }
}
