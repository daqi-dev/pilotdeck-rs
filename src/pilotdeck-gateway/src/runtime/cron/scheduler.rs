//! Port of TS `src/cron/runtime/CronScheduler.ts` (207 lines; line refs
//! below): the timer half of #2295 — start/stop/poke, the tick, and the
//! start-time recalculation.
//!
//! Timer model: TS arms a `setTimeout` per tick (60s idle poll —
//! `computeDelayMs` — never a precise next-run sleep) and filters by `isDue`
//! on every wake. The port's seat for that timer is one dedicated scheduler
//! thread waiting on a condvar whose `deadline` is `this.timer`: arming is
//! `deadline = now + waitMs` plus a notify, `clearTimeout` is a deadline
//! overwrite. Two TS races are deliberately absent: a poke during a running
//! tick can leak a second timer (both fire, ticks overlap) and a timer that
//! fired just before `stop()` can run one post-stop tick; the port serializes
//! ticks on the one thread and `stop()` is authoritative, so neither can
//! happen (stricter direction, registered in A8.3-CRON-SCHED-SERIAL-001).
use std::{
    sync::{Arc, Mutex, Weak},
    thread,
    time::{Duration, Instant},
};

use pilotdeck_protocol::{JsObject, JsString, JsValue};
use pilotdeck_transport::system_clock::resolve_local;

use super::fire::{CronFirePort, panic_message, snapshot_active_run, task_id_text};
use super::manager::{
    CronRuntime, computed_next_run_at_with, revision_of, spread_task, task_schedule_shape,
    timezone_str, value_string,
};
use super::schedule::{CRON_SCHEDULE_COMPUTATION_VERSION, once_computed_run_at};
use super::{
    ComputedRunAt, CronError, CronLogger, CronManagerConfig, CronTaskStore, CronZoneResolverCache,
    cron_day_fields_use_or, iso_from_epoch_ms, resolve_cron_timezone, strict_equals, truthy,
};

/// `DEFAULT_IDLE_POLL_MS` (CronScheduler.ts:8) — also the `delayTask`
/// deferral, which TS writes as this constant, not the injected poll.
pub const DEFAULT_IDLE_POLL_MS: u64 = 60_000;
/// `MIN_TIMER_MS` (CronScheduler.ts:9).
const MIN_TIMER_MS: u64 = 250;

/// `CronSchedulerDependencies` (CronScheduler.ts:11-21). `idle_poll_ms` is the
/// port's injectable seam for `computeDelayMs`'s constant (the golden
/// fixtures pin it low; production keeps [`DEFAULT_IDLE_POLL_MS`]).
pub struct SchedulerDeps {
    pub config: CronManagerConfig,
    pub store: CronTaskStore,
    pub fire: Arc<dyn CronFirePort>,
    pub uuid: Arc<dyn Fn() -> JsString + Send + Sync>,
    pub now: Arc<dyn Fn() -> f64 + Send + Sync>,
    /// `activeRunCount` (CronRuntime.ts:123): the manager-wide sum when the
    /// runtime has a manager, its own registry otherwise.
    pub active_run_count: Arc<dyn Fn() -> usize + Send + Sync>,
    pub logger: Arc<dyn CronLogger>,
    pub zone_cache: Arc<CronZoneResolverCache>,
    pub idle_poll_ms: u64,
    /// The owning runtime, held weakly so the scheduler thread exits once
    /// the runtime's world is gone; each fire spawn upgrades it once so an
    /// in-flight run keeps the runtime alive through its finally writes
    /// (TS GC keeps it alive through the fire promise chain).
    pub runtime: Weak<CronRuntime>,
}

/// TS `CronScheduler` (CronScheduler.ts:23). The handle is cheap to clone;
/// the thread behind `inner` is the single timer seat.
pub struct CronScheduler {
    inner: Arc<SchedulerInner>,
}

struct SchedulerInner {
    deps: SchedulerDeps,
    state: Mutex<TickState>,
    signal: std::sync::Condvar,
}

/// `this.timer`/`this.running`/`this.stopped` (CronScheduler.ts:24-27).
/// `deadline: None` is "no timer armed".
struct TickState {
    running: bool,
    stopped: bool,
    deadline: Option<Instant>,
    thread: Option<thread::JoinHandle<()>>,
}

impl CronScheduler {
    pub fn new(deps: SchedulerDeps) -> Result<Self, CronError> {
        Ok(Self {
            inner: Arc::new(SchedulerInner {
                deps,
                state: Mutex::new(TickState {
                    running: false,
                    stopped: false,
                    deadline: None,
                    thread: None,
                }),
                signal: std::sync::Condvar::new(),
            }),
        })
    }

    /// `start` (CronScheduler.ts:31-41). The disabled/stopped/running gates
    /// are early `return`s in TS; the recalculation runs on the caller before
    /// the first arm, and a recalculation failure rejects `start()` with
    /// `running` already set — exactly the TS state after the reject
    /// (`timer` undefined, nothing armed), because the timer seat is spawned
    /// *before* the recalculation: the thread waits unarmed, so a later
    /// `poke()` revives the scheduler like TS's `scheduleNextTick(0)`
    /// (CronScheduler.ts:55-64) instead of writing a deadline nobody
    /// consumes. A thread-spawn failure (EAGAIN) instead resets `running`
    /// before rejecting, so a later `start()` retries instead of
    /// early-returning a spurious Ok.
    pub fn start(&self) -> Result<(), CronError> {
        if !execution_allowed(&self.inner.deps) {
            return Ok(());
        }
        let enabled = self.inner.deps.config.enabled;
        {
            let mut state = lock(&self.inner.state);
            if !enabled || state.stopped || state.running {
                return Ok(());
            }
            state.running = true;
            // The seat spawns before the recalculation (the stopped gate
            // above just ran under the same lock, so no join can be missed);
            // a stop that lands while the lock is down for the recalculation
            // takes this handle with it and joins it, so no seat is leaked.
            if state.thread.is_none() {
                let inner = self.inner.clone();
                match thread::Builder::new()
                    .name("cron-scheduler".into())
                    .spawn(move || scheduler_thread(inner))
                {
                    Ok(handle) => state.thread = Some(handle),
                    Err(error) => {
                        // A spawn failure (EAGAIN/resource exhaustion)
                        // must not latch `running`: the gate above would
                        // swallow every later start() retry with a
                        // spurious Ok.
                        state.running = false;
                        drop(state);
                        self.inner.signal.notify_all();
                        return Err(CronError::error(JsString::from(error.to_string())));
                    }
                }
            }
        }
        let recalc = recalculate_all_next_runs(&self.inner.deps);
        let mut state = lock(&self.inner.state);
        match recalc {
            // The rejected start leaves the seat waiting unarmed — the exact
            // TS state, revived by a later poke.
            Err(error) => Err(error),
            Ok(()) => {
                // `scheduleNextTick()` — arm only when nothing is armed: a
                // poke during the recalc already set a closer deadline (TS
                // would leak both timers; the port keeps the earlier one).
                if !state.stopped && state.running && enabled && state.deadline.is_none() {
                    state.deadline = next_tick_deadline(self.inner.deps.idle_poll_ms);
                }
                self.inner.signal.notify_all();
                Ok(())
            }
        }
    }

    /// `stop` (CronScheduler.ts:43-53): flags, disarms, then waits out an
    /// in-flight tick (`await this.tickInProgress.catch(...)`). Never fails.
    pub fn stop(&self) {
        let handle = {
            let mut state = lock(&self.inner.state);
            state.stopped = true;
            state.running = false;
            state.deadline = None;
            state.thread.take()
        };
        self.inner.signal.notify_all();
        if let Some(handle) = handle {
            let _ = handle.join();
        }
    }

    /// `poke` (CronScheduler.ts:55-64): the three gates, then re-arm at the
    /// 250ms floor — overwriting a later deadline, as `clearTimeout` +
    /// `scheduleNextTick(0)` does.
    pub fn poke(&self) {
        let mut state = lock(&self.inner.state);
        if state.stopped || !state.running || !self.inner.deps.config.enabled {
            return;
        }
        state.deadline = Some(Instant::now() + Duration::from_millis(MIN_TIMER_MS));
        drop(state);
        self.inner.signal.notify_all();
    }

    /// `runTickOnce` (CronScheduler.ts:66-69): public for tests, not gated by
    /// `tickInProgress` — a tick run this way can overlap the thread's own
    /// tick exactly like the TS one.
    pub fn run_tick_once(&self) -> Result<(), CronError> {
        tick(&self.inner.deps)
    }

    /// The observation surface for the armed timer ("Public for tests",
    /// the `loaded_runtime`/`zone_resolver_cache` precedent): the pending
    /// delay, or `None` when no timer is armed.
    pub fn armed_tick_delay_ms(&self) -> Option<u64> {
        lock(&self.inner.state).deadline.map(|deadline| {
            deadline
                .saturating_duration_since(Instant::now())
                .as_millis() as u64
        })
    }

    /// Wakes the scheduler thread without changing any gate — the runtime's
    /// drop path uses it so the thread can notice the world is gone instead
    /// of waiting forever on an unarmed condvar.
    pub(super) fn wake(&self) {
        self.inner.signal.notify_all();
    }
}

/// `scheduleNextTick`'s wait computation (CronScheduler.ts:73): every arm
/// floors at `MIN_TIMER_MS` — an injected poll below the floor must not
/// re-arm at "now" and hot-loop `listTasks` — and an unrepresentable
/// `Instant + Duration` (std: "may panic" on overflow) is skipped instead of
/// panicking, mirroring TS's never-throwing `setTimeout`: the seat stays
/// unarmed until the next poke or start re-arms.
fn next_tick_deadline(idle_poll_ms: u64) -> Option<Instant> {
    Instant::now().checked_add(Duration::from_millis(idle_poll_ms.max(MIN_TIMER_MS)))
}

/// The timer seat: waits until the deadline, runs a tick, re-arms
/// (`scheduleNextTick`, CronScheduler.ts:71-86), forever until stopped or
/// unowned.
fn scheduler_thread(inner: Arc<SchedulerInner>) {
    loop {
        let mut state = lock(&inner.state);
        if state.stopped {
            return;
        }
        // The owning runtime is gone — no observer can distinguish this from
        // the process shutting down (TS GC collects the whole loop).
        if inner.deps.runtime.upgrade().is_none() {
            return;
        }
        let Some(deadline) = state.deadline else {
            state = inner.signal.wait(state).unwrap_or_else(|e| e.into_inner());
            continue;
        };
        let Some(wait) = deadline.checked_duration_since(Instant::now()) else {
            // The timer fired: this.timer = undefined, then the tick chain.
            state.deadline = None;
            drop(state);
            // TS folds a rejecting `tick()` into the warn and its `.then`
            // still re-arms (CronScheduler.ts:77-86); a panicking seam would
            // otherwise unwind this thread and leave `running` latched, so the
            // panic is caught and folded the same way — the next arm stands.
            let ticked = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                tick(&inner.deps)
            })) {
                Ok(result) => result,
                Err(panic) => Err(CronError::error(JsString::from(panic_message(&panic)))),
            };
            if let Err(error) = ticked {
                // "cron scheduler tick failed" (CronScheduler.ts:77-79);
                // the `.then` still re-arms below.
                let message = String::from_utf16_lossy(error.message().units());
                let mut meta = JsObject::new();
                meta.insert("error".into(), JsValue::from(message.as_str()));
                inner.deps.logger.warn("cron scheduler tick failed", &meta);
            }
            let mut state = lock(&inner.state);
            // `scheduleNextTick()` — arm only when nothing is armed: a poke
            // during the tick set a closer deadline that survives.
            if !state.stopped
                && state.running
                && inner.deps.config.enabled
                && state.deadline.is_none()
            {
                state.deadline = next_tick_deadline(inner.deps.idle_poll_ms);
            }
            continue;
        };
        let (guard, _) = inner
            .signal
            .wait_timeout(state, wait)
            .unwrap_or_else(|e| e.into_inner());
        state = guard;
        // Loop: a notify or spurious wake re-checks stopped/deadline.
    }
}

/// `tick` (CronScheduler.ts:92-110): list, filter `isDue`, then per task
/// either `delayTask` (capacity) or fire. A `delayTask` failure rejects the
/// whole tick and skips the remaining due tasks, exactly like the TS `await`
/// inside the loop.
fn execution_allowed(deps: &SchedulerDeps) -> bool {
    deps.runtime
        .upgrade()
        .is_none_or(|runtime| runtime.execution_allowed())
}

fn tick(deps: &SchedulerDeps) -> Result<(), CronError> {
    if !execution_allowed(deps) {
        return Ok(());
    }
    let now_ms = (deps.now)();
    let tasks = deps.store.list_tasks().map_err(CronError::from)?;
    for task in tasks.iter().filter(|task| is_due(task, now_ms)) {
        if (deps.active_run_count)() as f64 >= deps.config.max_concurrent_runs {
            delay_task(deps, task, now_ms)?;
            continue;
        }
        let run_id = (deps.uuid)();
        // TS registers the active run synchronously at the `void fire
        // .runTask(...)` call — before the fire's first await — so the tick
        // thread does the same; the fire's own register re-registers the same
        // runId, which merges (a stop requested in the window survives, the
        // runtime's register keeps `stop_requested`).
        let Some(runtime) = deps.runtime.upgrade() else {
            // The runtime went away mid-tick; the thread notices at the next
            // loop iteration. Nothing fired here ever reaches an observer.
            break;
        };
        runtime.register_active_run(snapshot_active_run(task, &run_id));
        let fire = deps.fire.clone();
        let logger = deps.logger.clone();
        let task = task.clone();
        let keepalive = runtime.clone();
        let fire_run_id = run_id.clone();
        let spawned = thread::Builder::new()
            .name("cron-fire".into())
            .spawn(move || {
                // The strong runtime handle keeps the finally writes
                // (outcome "stopped", post-run update) alive even after the
                // manager cleared its map.
                let _world = &keepalive;
                let result = fire.run_task(&task, &fire_run_id);
                if let Err(error) = result {
                    // "cron fire failed" (CronScheduler.ts:103-107).
                    let mut meta = JsObject::new();
                    meta.insert(
                        "taskId".into(),
                        task.get("taskId").cloned().unwrap_or(JsValue::Undefined),
                    );
                    meta.insert("runId".into(), JsValue::String(fire_run_id.clone()));
                    meta.insert(
                        "error".into(),
                        JsValue::from(String::from_utf16_lossy(error.message().units()).as_str()),
                    );
                    logger.warn("cron fire failed", &meta);
                }
            });
        if let Err(error) = spawned {
            runtime.unregister_active_run(&run_id);
            return Err(CronError::error(JsString::from(error.to_string())));
        }
    }
    Ok(())
}

/// `isDue` (CronScheduler.ts:190-199).
fn is_due(task: &JsValue, now_ms: f64) -> bool {
    if task
        .get("status")
        .and_then(JsValue::as_string)
        .is_some_and(|status| status.equals_str("running"))
    {
        return false;
    }
    if !truthy(task.get("nextRunAt").unwrap_or(&JsValue::Undefined)) {
        return false;
    }
    let Some(next_run_at) = task.get("nextRunAt").and_then(JsValue::as_string) else {
        // A truthy non-string `nextRunAt` cannot come from the store
        // (`normalize_task` keeps strings only), so this is a hand-seeded
        // snapshot and the strict-string fold is the recorded boundary.
        return false;
    };
    matches!(
        once_computed_run_at(next_run_at, &resolve_local),
        ComputedRunAt::Valid(ms) if ms <= now_ms
    )
}

/// `delayTask` (CronScheduler.ts:176-187): defer by the constant idle poll,
/// not the injected seam — a byte-exact 60s written to the store.
fn delay_task(deps: &SchedulerDeps, task: &JsValue, now_ms: f64) -> Result<(), CronError> {
    let Some(task_id) = task_id_text(task) else {
        // A non-string task id is outside normalized stored tasks.
        return Ok(());
    };
    let next_run_at = iso_from_epoch_ms(now_ms + DEFAULT_IDLE_POLL_MS as f64);
    deps.store
        .update_task_js(&task_id, |current| {
            if !matches_task_snapshot(current, task)
                || !current
                    .get("status")
                    .and_then(JsValue::as_string)
                    .is_some_and(|status| status.equals_str("scheduled"))
            {
                return Some(current.clone());
            }
            Some(JsValue::Object(spread_task(
                current,
                &[
                    ("nextRunAt", value_string(next_run_at.clone())),
                    ("revision", JsValue::Number(revision_of(current) + 1.0)),
                    ("updatedAt", value_string(iso_from_epoch_ms(now_ms))),
                ],
            )))
        })
        .map_err(CronError::from)?;
    Ok(())
}

/// `recalculateAllNextRuns` (CronScheduler.ts:112-174). TS fans the tasks out
/// through `Promise.all`; the per-task writes serialize on the store's task
/// lock either way, so the sequential fold is observably identical.
fn recalculate_all_next_runs(deps: &SchedulerDeps) -> Result<(), CronError> {
    if !execution_allowed(deps) {
        return Ok(());
    }
    let now_ms = (deps.now)();
    let tasks = deps.store.list_tasks().map_err(CronError::from)?;
    for task in &tasks {
        let schedule_type = task
            .get("schedule")
            .and_then(|schedule| schedule.get("type"))
            .and_then(JsValue::as_string);
        if schedule_type.is_some_and(|kind| kind.equals_str("once")) {
            // The once arm: skip a truthy cached run, else compute from the
            // raw stored schedule (fallback zone "UTC" — the once arm never
            // reads it), delete on no result.
            if truthy(task.get("nextRunAt").unwrap_or(&JsValue::Undefined)) {
                continue;
            }
            let schedule = task.get("schedule").cloned().unwrap_or(JsValue::Undefined);
            let shape = task_schedule_shape(&schedule, "UTC", now_ms)?;
            let Some((_, next_iso)) = computed_next_run_at_with(&shape, now_ms, &deps.zone_cache)?
            else {
                update_or_delete(deps, task, |current| {
                    if matches_task_snapshot(current, task) {
                        None
                    } else {
                        Some(current.clone())
                    }
                })?;
                continue;
            };
            update_or_delete(deps, task, |current| {
                if !matches_task_snapshot(current, task) {
                    return Some(current.clone());
                }
                Some(JsValue::Object(spread_task(
                    current,
                    &[
                        ("nextRunAt", value_string(next_iso.clone())),
                        ("revision", JsValue::Number(revision_of(current) + 1.0)),
                        ("updatedAt", value_string(iso_from_epoch_ms(now_ms))),
                    ],
                )))
            })?;
            continue;
        }

        // The cron arm (every other type lands here, as in TS): skip v3 with
        // a cached run, else resolve the zone and recompute.
        let version = task
            .get("scheduleComputationVersion")
            .and_then(JsValue::as_number);
        if version == Some(f64::from(CRON_SCHEDULE_COMPUTATION_VERSION))
            && truthy(task.get("nextRunAt").unwrap_or(&JsValue::Undefined))
        {
            continue;
        }
        let timezone = resolve_cron_timezone(
            timezone_str(task.get("schedule").and_then(|s| s.get("timezone"))).as_deref(),
            timezone_str(task.get("timezone")).as_deref(),
            Some(deps.config.timezone.as_str()),
        );
        let mut schedule = match task.get("schedule") {
            Some(JsValue::Object(object)) => object.clone(),
            // `{...task.schedule, timezone}` on a non-object is `{timezone}`.
            _ => JsObject::new(),
        };
        schedule.insert("timezone".into(), value_string(timezone.clone()));
        let schedule_value = JsValue::Object(schedule);
        // `const cachedRunAt = task.nextRunAt ? Date.parse(...) : NaN` —
        // the truthy gate first, then the same parser as `new Date`.
        let cached_run_at = match task.get("nextRunAt") {
            Some(value) if truthy(value) => match value {
                JsValue::String(text) => match once_computed_run_at(text, &resolve_local) {
                    ComputedRunAt::Valid(ms) => Some(ms),
                    _ => None,
                },
                _ => None,
            },
            _ => None,
        };
        let has_v2 = version == Some(2.0) && cached_run_at.is_some();
        // `hasV2CachedRun && (cachedRunAt <= now || !cronDayFieldsUseOr(...))`
        // — the `||` short-circuits: the expression check (and its
        // `expression.trim` TypeError on a non-string) only runs for a
        // future cache.
        let mut reuse_cached_run = has_v2 && cached_run_at.is_some_and(|cached| cached <= now_ms);
        if has_v2 && !reuse_cached_run {
            reuse_cached_run = !cron_day_fields_use_or_checked(&schedule_value)?;
        }
        let computed = if reuse_cached_run {
            None
        } else {
            let shape = task_schedule_shape(&schedule_value, &timezone, now_ms)?;
            computed_next_run_at_with(&shape, now_ms, &deps.zone_cache)?
        };
        // OR can bring a run forward, never push a pending v2 run back.
        let next_run_at = if has_v2
            && match &computed {
                None => true,
                Some((computed_ms, _)) => {
                    cached_run_at.is_some_and(|cached| cached <= *computed_ms)
                }
            } {
            task.get("nextRunAt").cloned().unwrap_or(JsValue::Undefined)
        } else {
            match &computed {
                Some((_, iso)) => value_string(iso.clone()),
                None => JsValue::Undefined,
            }
        };
        update_or_delete(deps, task, |current| {
            if !matches_task_snapshot(current, task) {
                return Some(current.clone());
            }
            Some(JsValue::Object(spread_task(
                current,
                &[
                    ("schedule", schedule_value.clone()),
                    ("timezone", value_string(timezone.clone())),
                    ("status", JsValue::from("scheduled")),
                    ("nextRunAt", next_run_at.clone()),
                    ("revision", JsValue::Number(revision_of(current) + 1.0)),
                    (
                        "scheduleComputationVersion",
                        JsValue::Number(f64::from(CRON_SCHEDULE_COMPUTATION_VERSION)),
                    ),
                    ("updatedAt", value_string(iso_from_epoch_ms(now_ms))),
                ],
            )))
        })?;
    }
    Ok(())
}

/// One `store.updateTask(task.taskId, matcher)` of the recalculation, with
/// the lone-surrogate taskId fold (the TS `===` never matches it).
fn update_or_delete(
    deps: &SchedulerDeps,
    task: &JsValue,
    matcher: impl Fn(&JsValue) -> Option<JsValue>,
) -> Result<(), CronError> {
    let Some(task_id) = task_id_text(task) else {
        return Ok(());
    };
    deps.store
        .update_task_js(&task_id, matcher)
        .map_err(CronError::from)?;
    Ok(())
}

/// `cronDayFieldsUseOr(schedule.expression)` (CronScheduler.ts:152) with the
/// `expression.trim is not a function` TypeError a non-string expression
/// throws (CronSchedule.ts:108).
fn cron_day_fields_use_or_checked(schedule: &JsValue) -> Result<bool, CronError> {
    match schedule.get("expression") {
        Some(JsValue::String(expression)) => Ok(cron_day_fields_use_or(expression)),
        _ => Err(CronError::type_error(JsString::from(
            "expression.trim is not a function",
        ))),
    }
}

/// `matchesTaskSnapshot` (CronScheduler.ts:201-206) — status compares
/// against the snapshot's own value, unlike the fire's
/// `matchesScheduledSnapshot` literal.
fn matches_task_snapshot(current: &JsValue, snapshot: &JsValue) -> bool {
    strict_equals(current.get("status"), snapshot.get("status"))
        && revision_of(current) == revision_of(snapshot)
        && strict_equals(current.get("nextRunAt"), snapshot.get("nextRunAt"))
        && strict_equals(current.get("lastRunId"), snapshot.get("lastRunId"))
}

/// Poisoned-lock recovery, the module's standing pattern.
fn lock(state: &Mutex<TickState>) -> std::sync::MutexGuard<'_, TickState> {
    state.lock().unwrap_or_else(|e| e.into_inner())
}
