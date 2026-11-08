//! Port of TS `src/cron/runtime/CronFire.ts` (295 lines; line refs below):
//! one cron run, from the store claim through the turn to the terminal
//! record and the post-run task update, on the synchronous `CronAgentGateway`
//! boundary.
//!
//! Event-stream boundary: TS `submitTurn` is an async generator whose events
//! `CronFire` consumes one by one (per-event `appendRunEvent`,
//! assistant-text accumulation, `interaction_required` aborts, error-event
//! classification, `forcedFailure` latching after a `turn_timeout`). The
//! Rust port's [`CronAgentGateway::submit_turn`] contract instead returns
//! only after the turn unwound, surfacing the first terminal `error` event as
//! `Err` with the event's code passed through untouched
//! (`native_process/assembly/cron.rs::failed_turn`), so the classification
//! below folds the in-loop error-event handling (CronFire.ts:125-140) into
//! the one visible event. The seams this boundary cannot represent —
//! assistant-text delivery for `deliverResult`, per-event `appendRunEvent`,
//! `interaction_required`/`forcedFailure` latching, `onPhaseEvent`,
//! `onTurnEvent` — are registered in the #2295 ledger, not silently
//! dropped.
use std::{cell::Cell, sync::Arc};

use pilotdeck_protocol::{JsObject, JsString, JsValue};

use super::manager::{
    computed_next_run_at_with, revision_of, spread_task, task_schedule_shape, timezone_str,
    to_display, value_string,
};
use super::{
    CRON_SCHEDULE_COMPUTATION_VERSION, CronActiveRun, CronAgentGateway, CronError, CronLogger,
    CronTaskStore, CronTurnSubmit, CronZoneResolverCache, iso_from_epoch_ms, resolve_cron_timezone,
    strict_equals,
};

/// `CronRunOutcome` (protocol/types.ts): the four terminal outcomes a run
/// record can carry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CronRunOutcome {
    Completed,
    Failed,
    Aborted,
    Stopped,
}
impl CronRunOutcome {
    fn as_str(self) -> &'static str {
        match self {
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Aborted => "aborted",
            Self::Stopped => "stopped",
        }
    }
}

/// The run record's `error` shape (CronFire.ts:113-116,129-131,139): a code
/// plus message pair, absent for completed runs.
struct CronRunError {
    code: String,
    message: JsString,
}
impl CronRunError {
    fn as_value(&self) -> JsValue {
        let mut error = JsObject::new();
        error.insert("code".into(), JsValue::from(self.code.as_str()));
        error.insert("message".into(), JsValue::String(self.message.clone()));
        JsValue::Object(error)
    }
}

/// `unregisterActiveRun`/`getActiveRun` (CronFire.ts:33-34): the runtime's
/// registry reads, keyed by run id.
type ActiveRunLookup = Arc<dyn Fn(&JsString) -> Option<CronActiveRun> + Send + Sync>;
/// `onResultDelivery` (CronFire.ts:38), optional.
type ResultDelivery = Arc<dyn Fn(&JsValue) -> Result<(), CronError> + Send + Sync>;

/// `CronFireDependencies` (CronFire.ts:28-44). The runtime closures are the
/// port's injectable seams, exactly the five TS wires; `onResultDelivery` is
/// optional and absent in the current native assembly (the app-level
/// delivery handler is not surfaced through `CronManagerOptions` yet —
/// ledger).
pub struct CronFireDeps {
    pub gateway: Arc<dyn CronAgentGateway>,
    pub store: CronTaskStore,
    pub now: Arc<dyn Fn() -> f64 + Send + Sync>,
    pub register_active_run: Arc<dyn Fn(CronActiveRun) + Send + Sync>,
    pub unregister_active_run: ActiveRunLookup,
    pub get_active_run: ActiveRunLookup,
    pub run_timeout_ms: f64,
    pub default_timezone: String,
    pub release_task_session: Arc<dyn Fn(&JsValue) + Send + Sync>,
    pub on_result_delivery: Option<ResultDelivery>,
    pub logger: Arc<dyn CronLogger>,
    pub zone_cache: Arc<CronZoneResolverCache>,
}

/// The scheduler-facing half of CronFire: `runTask(taskSnapshot, runId)`
/// (CronFire.ts:49). The scheduler calls it on its fire thread and maps an
/// `Err` to the `cron fire failed` warn (CronScheduler.ts:102-108).
pub trait CronFirePort: Send + Sync {
    fn run_task(&self, task: &JsValue, run_id: &JsString) -> Result<(), CronError>;
}

/// TS `CronFire` (CronFire.ts:46).
pub struct CronFire {
    deps: CronFireDeps,
}
impl CronFire {
    pub fn new(deps: CronFireDeps) -> Self {
        Self { deps }
    }

    /// `runTask` (CronFire.ts:49-203). The TS method never rejects: the catch
    /// folds every try-block failure into `outcome`/`error` and the finally's
    /// own awaits are each `.catch`-ed into warns. `Err` here is the port's
    /// mapping of a synchronous seam throw — a panic — which TS production
    /// cannot reach; the scheduler's `cron fire failed` warn is its consumer.
    pub fn run_task(&self, task_snapshot: &JsValue, run_id: &JsString) -> Result<(), CronError> {
        let attempt = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            self.run_task_inner(task_snapshot, run_id)
        }));
        match attempt {
            Ok(result) => result,
            Err(panic) => Err(CronError::error(JsString::from(panic_message(&panic)))),
        }
    }

    /// The try/catch/finally body, mirroring CronFire.ts:50-202.
    fn run_task_inner(&self, task_snapshot: &JsValue, run_id: &JsString) -> Result<(), CronError> {
        // const startedAt = this.deps.now() — read once, stamped on the
        // claim, the record, and nothing else.
        let started_at_ms = (self.deps.now)();
        // registerActiveRun before the try (CronFire.ts:58).
        (self.deps.register_active_run)(snapshot_active_run(task_snapshot, run_id));

        let mut task = task_snapshot.clone();
        let mut outcome = CronRunOutcome::Completed;
        let mut error: Option<CronRunError> = None;
        let mut started_run = false;

        // The try block (CronFire.ts:67-141): the claim, then the turn.
        // `forcedFailure` stays false on this boundary — its only TS setters
        // (`turn_timeout`/`interaction_required`) are folded into the
        // classification below — so the catch arm is the plain seam fold.
        let claimed = Cell::new(false);
        // The lone-surrogate taskId seam: the TS store compares `===` on raw
        // values, but a persisted taskId round-trips through lossy UTF-8, so
        // a lone surrogate never matches a stored task on either side.
        let claim = match task_id_text(task_snapshot) {
            Some(task_id) => self
                .deps
                .store
                .update_task(&task_id, |current| {
                    if !matches_scheduled_snapshot(current, task_snapshot) {
                        return Some(current.clone());
                    }
                    claimed.set(true);
                    Some(JsValue::Object(spread_task(
                        current,
                        &[
                            ("status", JsValue::from("running")),
                            ("lastRunId", JsValue::String(run_id.clone())),
                            ("revision", JsValue::Number(revision_of(current) + 1.0)),
                            ("updatedAt", value_string(iso_from_epoch_ms(started_at_ms))),
                        ],
                    )))
                })
                .map_err(CronError::from),
            None => Ok(None),
        };
        match claim {
            Ok(Some(current_task)) if claimed.get() => {
                task = current_task;
                started_run = true;
                let submit = CronTurnSubmit {
                    session_key: task_string(&task, "sessionKey").unwrap_or_default(),
                    channel_key: task_string(&task, "channelKey").unwrap_or_default(),
                    project_key: task_string(&task, "projectKey"),
                    message: task_string(&task, "message").unwrap_or_default(),
                    // CronFire.ts:101: mode is the literal, never task.mode.
                    mode: JsString::from("bypassPermissions"),
                    run_id: run_id.clone(),
                    timeout_ms: self.deps.run_timeout_ms,
                };
                if let Err(caught) = self.deps.gateway.submit_turn(&submit) {
                    // The error-event classification (CronFire.ts:126-139):
                    // the boundary carries the event's code through, and a
                    // code-less error is a thrown seam error
                    // (CronFire.ts:142-149).
                    match caught.code() {
                        Some("turn_timeout") => {
                            outcome = CronRunOutcome::Failed;
                            error = Some(CronRunError {
                                code: "cron_run_timeout".into(),
                                message: caught.message().clone(),
                            });
                        }
                        Some(code) if code == "agent_aborted" => {
                            outcome = CronRunOutcome::Aborted;
                            error = Some(CronRunError {
                                code: code.to_owned(),
                                message: caught.message().clone(),
                            });
                        }
                        Some(code) => {
                            outcome = CronRunOutcome::Failed;
                            error = Some(CronRunError {
                                code: code.to_owned(),
                                message: caught.message().clone(),
                            });
                        }
                        None => {
                            outcome = CronRunOutcome::Failed;
                            error = Some(CronRunError {
                                code: "cron_run_failed".into(),
                                message: caught.message().clone(),
                            });
                        }
                    }
                }
            }
            // `!claimed || !currentTask` (CronFire.ts:82-85).
            Ok(_) => outcome = CronRunOutcome::Aborted,
            // The catch arm (CronFire.ts:142-149): the finally's early
            // return keeps this run record-less, exactly like a claim the
            // store rejected.
            Err(caught) => {
                outcome = CronRunOutcome::Failed;
                error = Some(CronRunError {
                    code: "cron_run_failed".into(),
                    message: caught.message().clone(),
                });
            }
        }

        // The finally block (CronFire.ts:150-202) runs on every path.
        // getActiveRun(runId)?.stopRequested → outcome = "stopped"
        // (CronFire.ts:151-154).
        if (self.deps.get_active_run)(run_id).is_some_and(|run| run.stop_requested) {
            outcome = CronRunOutcome::Stopped;
        }
        (self.deps.unregister_active_run)(run_id);
        if !started_run {
            // `return` inside finally (CronFire.ts:156-158): no record, no
            // delivery, no post-run update — the run never started.
            return Ok(());
        }
        let finished_at_ms = (self.deps.now)();

        // appendRun (CronFire.ts:160-178), failure only warns.
        let mut record = JsObject::new();
        record.insert("schemaVersion".into(), JsValue::Number(1.0));
        record.insert("runId".into(), JsValue::String(run_id.clone()));
        record.insert("taskId".into(), task_field(&task, "taskId"));
        record.insert("sessionKey".into(), task_field(&task, "sessionKey"));
        record.insert("projectKey".into(), task_field(&task, "projectKey"));
        record.insert(
            "startedAt".into(),
            value_string(iso_from_epoch_ms(started_at_ms)),
        );
        record.insert(
            "finishedAt".into(),
            value_string(iso_from_epoch_ms(finished_at_ms)),
        );
        record.insert("outcome".into(), JsValue::from(outcome.as_str()));
        if let Some(error) = &error {
            record.insert("error".into(), error.as_value());
        }
        if let Err(store_error) = self.deps.store.append_run(&JsValue::Object(record)) {
            fire_warn(
                &self.deps.logger,
                "cron run terminal record write failed",
                &task,
                run_id,
                &store_error.to_string(),
            );
        }

        // deliverResult (CronFire.ts:188-194 via 217-240), failure only warns.
        if let Err(delivery_error) = self.deliver_result(&task, run_id, outcome, error.as_ref()) {
            fire_warn(
                &self.deps.logger,
                "cron result delivery failed",
                &task,
                run_id,
                &delivery_error,
            );
        }

        // updateTaskAfterRun (CronFire.ts:195-201 via 242-280), failure only
        // warns.
        if let Err(update_error) = self.update_task_after_run(&task, run_id, finished_at_ms) {
            fire_warn(
                &self.deps.logger,
                "cron task post-run update failed",
                &task,
                run_id,
                &String::from_utf16_lossy(update_error.message().units()),
            );
        }
        Ok(())
    }

    /// `deliverResult` (CronFire.ts:217-240). The completed arm's text is the
    /// accumulated assistant text on the TS side; this boundary consumes no
    /// event stream, so it is always empty and takes the fixed fallback.
    fn deliver_result(
        &self,
        task: &JsValue,
        run_id: &JsString,
        outcome: CronRunOutcome,
        error: Option<&CronRunError>,
    ) -> Result<(), String> {
        let text = if outcome == CronRunOutcome::Completed {
            String::new()
        } else {
            let message = error
                .map(|error| String::from_utf16_lossy(error.message.units()))
                .unwrap_or_default();
            let trimmed = crate::metadata::js_trim(&message).to_owned();
            if trimmed.is_empty() {
                "Cron task failed.".to_owned()
            } else {
                trimmed
            }
        };
        let delivery_text = if text.is_empty() {
            "定时任务已完成，但没有返回内容。"
        } else {
            &text
        };
        let Some(deliver) = self.deps.on_result_delivery.as_ref() else {
            // `onResultDelivery?.(...)`: absent is a no-op that never rejects.
            return Ok(());
        };
        let mut delivery = JsObject::new();
        delivery.insert("taskId".into(), task_field(task, "taskId"));
        delivery.insert("runId".into(), JsValue::String(run_id.clone()));
        delivery.insert("sessionKey".into(), task_field(task, "sessionKey"));
        delivery.insert("channelKey".into(), task_field(task, "channelKey"));
        delivery.insert(
            "originSessionKey".into(),
            task_field(task, "originSessionKey"),
        );
        delivery.insert(
            "originChannelKey".into(),
            task_field(task, "originChannelKey"),
        );
        delivery.insert("projectKey".into(), task_field(task, "projectKey"));
        delivery.insert("outcome".into(), JsValue::from(outcome.as_str()));
        delivery.insert("text".into(), JsValue::from(delivery_text));
        if let Some(error) = error {
            delivery.insert("error".into(), error.as_value());
        }
        deliver(&JsValue::Object(delivery))
            .map_err(|error| String::from_utf16_lossy(error.message().units()))
    }

    /// `updateTaskAfterRun` (CronFire.ts:242-280).
    fn update_task_after_run(
        &self,
        task: &JsValue,
        run_id: &JsString,
        finished_at_ms: f64,
    ) -> Result<(), CronError> {
        let once = task
            .get("schedule")
            .and_then(|schedule| schedule.get("type"))
            .and_then(JsValue::as_string)
            .is_some_and(|kind| kind.equals_str("once"));
        let Some(task_id) = task_id_text(task) else {
            // A non-UTF-8 taskId cannot cross the &str store seam; the TS
            // updateTask would compare it === against itself and match, so
            // this is the lone-surrogate seam, not a semantic skip.
            return Ok(());
        };
        if once {
            // matchesRunningTask → delete, then releaseTaskSession
            // (CronFire.ts:243-255).
            let deleted = Cell::new(false);
            self.deps.store.update_task(&task_id, |current| {
                if !matches_running_task(current, task, run_id) {
                    return Some(current.clone());
                }
                deleted.set(true);
                None
            })?;
            if deleted.get() {
                (self.deps.release_task_session)(task);
            }
            return Ok(());
        }
        // The cron arm (CronFire.ts:257-278): resolve the zone, spread it
        // into the stored schedule, recompute from finishedAt. `{...task
        // .schedule, timezone}` on a non-object schedule spreads to `{}`.
        let timezone = resolve_cron_timezone(
            timezone_str(task.get("schedule").and_then(|s| s.get("timezone"))).as_deref(),
            timezone_str(task.get("timezone")).as_deref(),
            Some(self.deps.default_timezone.as_str()),
        );
        let mut schedule = match task.get("schedule") {
            Some(JsValue::Object(object)) => object.clone(),
            _ => JsObject::new(),
        };
        schedule.insert("timezone".into(), value_string(timezone.clone()));
        let schedule_value = JsValue::Object(schedule);
        let shape = task_schedule_shape(&schedule_value, &timezone, finished_at_ms)?;
        let next_run_at =
            match computed_next_run_at_with(&shape, finished_at_ms, &self.deps.zone_cache)? {
                Some((_, iso)) => value_string(iso),
                None => JsValue::Undefined,
            };
        self.deps.store.update_task(&task_id, |current| {
            if !matches_running_task(current, task, run_id) {
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
                    ("updatedAt", value_string(iso_from_epoch_ms(finished_at_ms))),
                ],
            )))
        })?;
        Ok(())
    }
}

impl CronFirePort for CronFire {
    fn run_task(&self, task: &JsValue, run_id: &JsString) -> Result<(), CronError> {
        CronFire::run_task(self, task, run_id)
    }
}

/// `matchesScheduledSnapshot` (CronFire.ts:283-288). The status is compared
/// against the literal `"scheduled"`, not against the snapshot's own status.
fn matches_scheduled_snapshot(current: &JsValue, snapshot: &JsValue) -> bool {
    current
        .get("status")
        .and_then(JsValue::as_string)
        .is_some_and(|status| status.equals_str("scheduled"))
        && revision_of(current) == revision_of(snapshot)
        && strict_equals(current.get("nextRunAt"), snapshot.get("nextRunAt"))
        && strict_equals(current.get("lastRunId"), snapshot.get("lastRunId"))
}

/// `matchesRunningTask` (CronFire.ts:290-294).
fn matches_running_task(current: &JsValue, claimed_task: &JsValue, run_id: &JsString) -> bool {
    current
        .get("status")
        .and_then(JsValue::as_string)
        .is_some_and(|status| status.equals_str("running"))
        && strict_equals(
            current.get("lastRunId"),
            Some(&JsValue::String(run_id.clone())),
        )
        && revision_of(current) == revision_of(claimed_task)
}

/// The `CronActiveRun` a task snapshot registers (CronFire.ts:51-57),
/// factored out because the scheduler pre-registers the same shape at the
/// `void fire.runTask(...)` call site — exactly where TS's synchronous
/// register happens, before the fire's first await.
pub(super) fn snapshot_active_run(task_snapshot: &JsValue, run_id: &JsString) -> CronActiveRun {
    CronActiveRun {
        run_id: run_id.clone(),
        task_id: task_string(task_snapshot, "taskId").unwrap_or_default(),
        session_key: task_string(task_snapshot, "sessionKey").unwrap_or_default(),
        // `scheduleType: taskSnapshot.schedule.type` (CronFire.ts:55): a
        // non-string stored type never equals "once" later, and the
        // `String()` fold cannot manufacture "once" out of one, so the
        // display form is observationally the raw value.
        schedule_type: task_snapshot
            .get("schedule")
            .and_then(|schedule| schedule.get("type"))
            .and_then(|kind| to_display(kind).ok())
            .unwrap_or_default(),
        stop_requested: false,
    }
}

/// The `taskId` of a stored task as UTF-8, or `None` when it cannot cross the
/// store's `&str` seam (a lone surrogate).
pub(super) fn task_id_text(task: &JsValue) -> Option<String> {
    task.get("taskId")
        .and_then(JsValue::as_string)
        .and_then(|task_id| task_id.to_utf8().ok())
}

/// A stored string field, `undefined` when absent.
fn task_field(task: &JsValue, field: &str) -> JsValue {
    task.get(field).cloned().unwrap_or(JsValue::Undefined)
}

/// A stored string field as an owned `JsString` (empty for absent/non-string —
/// the store's `normalize_task` guarantees the ones the turn needs are
/// strings, so the fallback is only a non-store snapshot in tests).
fn task_string(task: &JsValue, field: &str) -> Option<JsString> {
    match task.get(field) {
        Some(JsValue::String(text)) => Some(text.clone()),
        _ => None,
    }
}

/// The warn metadata TS builds at every fire seam (CronFire.ts:173-177,
/// 189-193, 196-200): `{taskId, runId, error}` with the error's message.
fn fire_warn(
    logger: &Arc<dyn CronLogger>,
    message: &str,
    task: &JsValue,
    run_id: &JsString,
    error: &str,
) {
    let mut meta = JsObject::new();
    meta.insert("taskId".into(), task_field(task, "taskId"));
    meta.insert("runId".into(), JsValue::String(run_id.clone()));
    meta.insert("error".into(), JsValue::from(error));
    logger.warn(message, &meta);
}

/// `error instanceof Error ? error.message : String(error)` for a Rust panic
/// payload standing in for a thrown non-Error value.
fn panic_message(panic: &Box<dyn std::any::Any + Send>) -> String {
    if let Some(text) = panic.downcast_ref::<&str>() {
        (*text).to_owned()
    } else if let Some(text) = panic.downcast_ref::<String>() {
        text.clone()
    } else {
        "non-string panic payload".to_owned()
    }
}
