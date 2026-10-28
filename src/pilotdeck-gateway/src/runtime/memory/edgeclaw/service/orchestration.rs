//! `EdgeClawMemoryService` asynchronous orchestration (A6.6-edgeclaw-19).
//!
//! Ports `service.ts:355-412,674-735,776-887,915-969` of the fixed TS baseline:
//!
//! * the timestamp helpers `parseTimestamp` / `hasElapsedMinutes` /
//!   `buildLastDreamSnapshotMetadata` (`:355-412`);
//! * `getPipelineTimestamp` / `setPipelineTimestamp` and the auto-index /
//!   auto-dream anchor reconciliation (`:674-735`);
//! * `flush` (manual anchor → heartbeat → both anchors → retriever cache),
//!   `dream` (manual anchor → prep flush → dream stage → install/replace →
//!   state → trace → anchors → manifest repair) and the synchronous
//!   `rollbackLastDream` (`:776-887`);
//! * `runDueScheduledMaintenance` (20-turn backlog threshold, interval gating,
//!   `startsWith("scheduled")` reason composition, interval 0 disabled)
//!   (`:915-969`).
//!
//! `retrieve*` (`:889-913`) stays in the retrieval package.
//!
//! Concurrency follows the M19 matrix probe (`_tmp/20261026-issue-776/
//! MATRIX.md`, Node v22.22.0 against cc7c4e6): TS interleaves events at
//! `await` points on the single event loop, so every synchronous segment here
//! is atomic and the observable interleaving happens at the injected
//! transport's completion, matching the TS fetch-gate probe cell for cell.

use super::{EdgeClawMemoryService, Result as ServiceResult, ServiceError, ServiceLogger};
use crate::runtime::memory::edgeclaw::{
    dream::{
        run::DreamExecutionResult,
        runner::{DreamEnvironment, DreamLogger, DreamRewriteRunner, DreamRunnerError},
    },
    file_memory::FileMemoryError,
    heartbeat::indexer::{HeartbeatRunOptions, HeartbeatStats},
    storage::js_truthy,
};
use crate::runtime::memory::js_trim_units;
use pilotdeck_protocol::{JsObject, JsString, JsValue};
use std::{
    path::Path,
    sync::{Arc, atomic::Ordering},
};

const AUTO_INDEX_ANCHOR_AT_STATE_KEY: &str = "autoIndexAnchorAt";
const AUTO_DREAM_ANCHOR_AT_STATE_KEY: &str = "autoDreamAnchorAt";
const LAST_DREAM_AT_STATE_KEY: &str = "lastDreamAt";
const LAST_INDEXED_AT_STATE_KEY: &str = "lastIndexedAt";
const LAST_DREAM_STATUS_STATE_KEY: &str = "lastDreamStatus";
const LAST_DREAM_SUMMARY_STATE_KEY: &str = "lastDreamSummary";
const DREAM_TRACE_LIMIT: usize = 30;
/// `AUTO_INDEX_PENDING_DIALOGUE_TURN_THRESHOLD` (`service.ts:99`).
const AUTO_INDEX_PENDING_DIALOGUE_TURN_THRESHOLD: i64 = 20;

/// `Math.random` seam for the dream environment. Production draws from a
/// fresh UUID-backed xorshift; fixtures cycle the frozen TS values.
pub type MathRandom = Arc<dyn Fn() -> f64 + Send + Sync>;

/// Production `Math.random()`: each call reseeds from a fresh uuid so
/// successive draws are independent, like the V8 generator.
pub fn default_math_random() -> f64 {
    let bytes = uuid::Uuid::new_v4();
    let mut state = u64::from_le_bytes(bytes.as_bytes()[..8].try_into().expect("#776 uuid bytes"));
    state ^= state << 13;
    state ^= state >> 7;
    state ^= state << 17;
    (state >> 11) as f64 / (1u64 << 53) as f64
}

/// `flush(options)` (`service.ts:776-797`).
#[derive(Default)]
pub struct FlushOptions {
    pub batch_size: Option<usize>,
    pub session_keys: Option<Vec<String>>,
    pub reason: Option<JsString>,
}

/// `dream(trigger)` (`service.ts:799`): only the two TS call sites exist.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DreamTrigger {
    Manual,
    Scheduled,
}
impl DreamTrigger {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Manual => "manual",
            Self::Scheduled => "scheduled",
        }
    }
}

/// `DreamRunResult` (`service.ts:871-877`), insertion order included.
#[derive(Clone, Debug)]
pub struct DreamRunOutcome {
    pub prep_flush: HeartbeatStats,
    pub reviewed_files: f64,
    pub rewritten_projects: f64,
    pub deleted_projects: f64,
    pub deleted_files: f64,
    pub profile_updated: bool,
    pub duplicate_topic_count: f64,
    pub conflict_topic_count: f64,
    pub summary: JsString,
    pub trigger: DreamTrigger,
    pub status: &'static str,
}
impl DreamRunOutcome {
    pub fn to_value(&self) -> JsValue {
        let mut object = JsObject::new();
        object.insert("prepFlush".into(), self.prep_flush.to_value());
        for (key, value) in [
            ("reviewedFiles", self.reviewed_files),
            ("rewrittenProjects", self.rewritten_projects),
            ("deletedProjects", self.deleted_projects),
            ("deletedFiles", self.deleted_files),
            ("duplicateTopicCount", self.duplicate_topic_count),
            ("conflictTopicCount", self.conflict_topic_count),
        ] {
            object.insert(key.into(), JsValue::Number(value));
        }
        object.insert("profileUpdated".into(), JsValue::Bool(self.profile_updated));
        object.insert("summary".into(), JsValue::String(self.summary.clone()));
        object.insert(
            "trigger".into(),
            JsValue::String(self.trigger.as_str().into()),
        );
        object.insert("status".into(), JsValue::String(self.status.into()));
        JsValue::Object(object)
    }
}

/// `runDueScheduledMaintenance` return (`service.ts:916-920,963-969`): the
/// optional members are spread only when their call ran.
#[derive(Clone, Debug, Default)]
pub struct MaintenanceOutcome {
    pub index_ran: bool,
    pub dream_ran: bool,
    pub index_stats: Option<HeartbeatStats>,
    pub dream_result: Option<DreamRunOutcome>,
}
impl MaintenanceOutcome {
    pub fn to_value(&self) -> JsValue {
        let mut object = JsObject::new();
        object.insert("indexRan".into(), JsValue::Bool(self.index_ran));
        object.insert("dreamRan".into(), JsValue::Bool(self.dream_ran));
        if let Some(stats) = &self.index_stats {
            object.insert("indexStats".into(), stats.to_value());
        }
        if let Some(result) = &self.dream_result {
            object.insert("dreamResult".into(), result.to_value());
        }
        JsValue::Object(object)
    }
}

/// `parseTimestamp` (`service.ts:355-358`): `Date.parse(value ?? "")` — NaN
/// becomes `None`. The V8 parser is `parse_node22_date`; its `Err` marks a
/// local-wall-time input the port cannot resolve, which TS would answer from
/// the ambient timezone, so it surfaces instead of silently skipping.
fn parse_timestamp(value: Option<&JsString>) -> ServiceResult<Option<f64>> {
    let input = value.cloned().unwrap_or_default();
    pilotdeck_transport::system_clock::parse_node22_date(&input).map_err(|message| {
        ServiceError::File(FileMemoryError::Invalid(String::from_utf16_lossy(
            message.units(),
        )))
    })
}

/// `hasElapsedMinutes` (`service.ts:360-369`). `intervalMinutes <= 0` and a
/// null anchor both disable the check; the comparison is JS `f64` arithmetic.
fn has_elapsed_minutes(
    anchor_at: Option<&JsString>,
    interval_minutes: f64,
    now_ms: f64,
) -> ServiceResult<bool> {
    if interval_minutes <= 0.0 {
        return Ok(false);
    }
    let Some(anchor_ms) = parse_timestamp(anchor_at)? else {
        return Ok(false);
    };
    Ok(now_ms - anchor_ms >= interval_minutes * 60_000.0)
}

/// `buildLastDreamSnapshotMetadata` (`service.ts:374-412`) with the exact
/// object-literal insertion order: `metadata.json` bytes are part of the
/// golden tree. The conditional spreads keep `trigger` / `dreamTraceId` /
/// `summary` out when falsy, exactly like TS.
fn build_last_dream_snapshot_metadata(
    workspace_dir: &Path,
    captured_at: &JsString,
    before: &JsValue,
    after: &JsValue,
    trigger: DreamTrigger,
    dream_trace_id: &JsValue,
    summary: &JsString,
) -> JsValue {
    let field = |value: &JsValue, key: &str| value.get(key).cloned().unwrap_or(JsValue::Undefined);
    let stage = |value: &JsValue| {
        let mut object = JsObject::new();
        object.insert("workspaceVersion".into(), field(value, "workspaceVersion"));
        object.insert("globalVersion".into(), field(value, "globalVersion"));
        object.insert("counts".into(), field(value, "counts"));
        object.insert("runtimeState".into(), field(value, "runtimeState"));
        JsValue::Object(object)
    };
    let mut object = JsObject::new();
    object.insert("version".into(), JsValue::Number(1.0));
    object.insert("capturedAt".into(), JsValue::String(captured_at.clone()));
    object.insert("sourceAction".into(), JsValue::String("dream".into()));
    object.insert(
        "sourceWorkspaceDir".into(),
        JsValue::String(workspace_dir.to_string_lossy().into_owned().into()),
    );
    let trigger_string = JsString::from(trigger.as_str());
    if !trigger_string.is_empty() {
        object.insert("trigger".into(), JsValue::String(trigger_string));
    }
    if js_truthy(dream_trace_id) {
        object.insert("dreamTraceId".into(), dream_trace_id.clone());
    }
    if !summary.is_empty() {
        object.insert("summary".into(), JsValue::String(summary.clone()));
    }
    object.insert("before".into(), stage(before));
    object.insert("after".into(), stage(after));
    JsValue::Object(object)
}

/// The dream environment the service hands to the staged runner: the same
/// `nowIso` clock the repository uses, `Date.now` as `nowMs`, `Math.random`
/// as `random`, and the trace suffix from the injected token seam. TS reads
/// the globals directly (`dream-review.ts:138-140,209`); the seams exist so
/// the fixture replay can freeze all four.
struct ServiceDreamEnvironment<'a> {
    service: &'a EdgeClawMemoryService,
}
impl DreamEnvironment for ServiceDreamEnvironment<'_> {
    fn now_ms(&self) -> f64 {
        (self.service.now_ms)() as f64
    }
    fn random(&self) -> f64 {
        (self.service.math_random)()
    }
    fn now_iso(&self) -> JsString {
        (self.service.clock)().into()
    }
    /// `Math.random().toString(36).slice(2, 10)`. V8's shortest round-trip
    /// radix-36 formatting stays at the JS boundary: consume the same draw
    /// the TS call makes, then return the injected token, which the fixture
    /// sets to the exact TS suffix bytes.
    fn random_suffix(&self) -> JsString {
        let _ = self.random();
        (self.service.random_token)()
    }
}

/// `options.logger` reaching `DreamRewriteRunner` (`service.ts:819-822`).
struct DreamLoggerAdapter(Arc<dyn ServiceLogger>);
impl DreamLogger for DreamLoggerAdapter {
    fn info(&self, message: &str) {
        self.0.info(&JsString::from(message));
    }
    fn warn(&self, message: &JsString) {
        self.0.warn(message);
    }
    fn error(&self, message: &str) {
        self.0.error(&JsString::from(message));
    }
}

/// `DreamRunnerError` → [`ServiceError`]: the two TS failure surfaces (store
/// and LLM) both reach the caller as plain `Error` instances.
impl From<DreamRunnerError> for ServiceError {
    fn from(error: DreamRunnerError) -> Self {
        match error {
            DreamRunnerError::Memory(error) => Self::File(error),
            DreamRunnerError::Llm(error) => Self::Llm(error),
        }
    }
}

impl EdgeClawMemoryService {
    /// `getPipelineTimestamp` (`service.ts:674-677`): a string that trims to
    /// non-empty, else `undefined`.
    fn get_pipeline_timestamp(&self, key: &str) -> ServiceResult<Option<JsString>> {
        let value = self.repository.storage.get_pipeline_state(key)?;
        Ok(value
            .as_ref()
            .and_then(JsValue::as_string)
            .filter(|value| !js_trim_units(value.units()).is_empty())
            .cloned())
    }

    /// `setPipelineTimestamp` (`service.ts:679-684`): set when the trimmed
    /// value is non-empty, delete otherwise.
    fn set_pipeline_timestamp(&self, key: &str, value: Option<&JsString>) -> ServiceResult<()> {
        match value.filter(|value| !js_trim_units(value.units()).is_empty()) {
            Some(value) => self
                .repository
                .storage
                .set_pipeline_state(key, &JsValue::String(value.clone()))?,
            None => self.repository.storage.delete_pipeline_state(key)?,
        }
        Ok(())
    }

    /// `reconcileAutoIndexAnchor` (`service.ts:687-706`).
    fn reconcile_auto_index_anchor(
        &self,
        manual_reset_at: Option<&JsString>,
        session_keys: Option<&[String]>,
    ) -> ServiceResult<Option<JsString>> {
        if let Some(manual_reset_at) = manual_reset_at {
            self.set_pipeline_timestamp(AUTO_INDEX_ANCHOR_AT_STATE_KEY, Some(manual_reset_at))?;
            return Ok(Some(manual_reset_at.clone()));
        }
        let first_pending_at = self
            .repository
            .storage
            .get_earliest_pending_timestamp(session_keys)?
            .map(JsString::from);
        let Some(first_pending_at) = first_pending_at else {
            self.set_pipeline_timestamp(AUTO_INDEX_ANCHOR_AT_STATE_KEY, None)?;
            return Ok(None);
        };
        if let Some(existing) = self.get_pipeline_timestamp(AUTO_INDEX_ANCHOR_AT_STATE_KEY)? {
            return Ok(Some(existing));
        }
        self.set_pipeline_timestamp(AUTO_INDEX_ANCHOR_AT_STATE_KEY, Some(&first_pending_at))?;
        Ok(Some(first_pending_at))
    }

    /// `reconcileAutoDreamAnchor` (`service.ts:708-735`).
    fn reconcile_auto_dream_anchor(
        &self,
        manual_reset_at: Option<&JsString>,
        fallback_at: Option<&JsString>,
    ) -> ServiceResult<Option<JsString>> {
        let last_dream_at = self.get_pipeline_timestamp(LAST_DREAM_AT_STATE_KEY)?;
        let last_indexed_at = self.get_pipeline_timestamp(LAST_INDEXED_AT_STATE_KEY)?;
        let mut file_input = JsObject::new();
        if let Some(last_dream_at) = &last_dream_at {
            file_input.insert("lastDreamAt".into(), JsValue::String(last_dream_at.clone()));
        }
        let file_overview = self
            .repository
            .get_file_memory_store()
            .get_overview(&file_input)?;
        let changed_files_since_last_dream = match file_overview.get("changedFilesSinceLastDream") {
            Some(JsValue::Number(value)) => *value,
            _ => 0.0,
        };
        if last_indexed_at.is_none() || changed_files_since_last_dream <= 0.0 {
            self.set_pipeline_timestamp(AUTO_DREAM_ANCHOR_AT_STATE_KEY, None)?;
            return Ok(None);
        }
        if let Some(manual_reset_at) = manual_reset_at {
            self.set_pipeline_timestamp(AUTO_DREAM_ANCHOR_AT_STATE_KEY, Some(manual_reset_at))?;
            return Ok(Some(manual_reset_at.clone()));
        }
        if let Some(existing) = self.get_pipeline_timestamp(AUTO_DREAM_ANCHOR_AT_STATE_KEY)? {
            return Ok(Some(existing));
        }
        let next_anchor = fallback_at.or(last_indexed_at.as_ref());
        self.set_pipeline_timestamp(AUTO_DREAM_ANCHOR_AT_STATE_KEY, next_anchor)?;
        Ok(next_anchor.cloned())
    }

    /// `flush` (`service.ts:776-797`). The heartbeat runs on a clone of the
    /// indexer so the mutex is never held across an LLM await.
    pub async fn flush(&self, options: FlushOptions) -> ServiceResult<HeartbeatStats> {
        let manual_reset_at = if options
            .reason
            .as_ref()
            .is_some_and(|reason| reason.equals_str("manual"))
        {
            Some(JsString::from((self.clock)()))
        } else {
            None
        };
        if let Some(manual_reset_at) = &manual_reset_at {
            self.reconcile_auto_index_anchor(
                Some(manual_reset_at),
                options.session_keys.as_deref(),
            )?;
        }
        let indexer = self
            .indexer
            .lock()
            .expect("edgeclaw service indexer mutex poisoned")
            .clone();
        let stats = indexer
            .run_heartbeat(HeartbeatRunOptions {
                batch_size: options.batch_size,
                session_keys: options.session_keys.clone(),
                reason: options.reason.clone(),
            })
            .await?;
        self.reconcile_auto_index_anchor(None, options.session_keys.as_deref())?;
        if stats.written_files > 0 {
            let fallback_at = JsString::from((self.clock)());
            self.reconcile_auto_dream_anchor(None, Some(&fallback_at))?;
        } else {
            self.reconcile_auto_dream_anchor(None, None)?;
        }
        self.retriever.reset_transient_state();
        Ok(stats)
    }

    /// `dream` (`service.ts:799-877`).
    pub async fn dream(&self, trigger: DreamTrigger) -> ServiceResult<DreamRunOutcome> {
        if trigger == DreamTrigger::Manual {
            let manual_reset_at = JsString::from((self.clock)());
            self.reconcile_auto_dream_anchor(Some(&manual_reset_at), None)?;
        }
        let prep_flush = self
            .flush(FlushOptions {
                reason: Some(JsString::from(match trigger {
                    DreamTrigger::Manual => "manual_dream_prep",
                    DreamTrigger::Scheduled => "scheduled_dream_prep",
                })),
                ..FlushOptions::default()
            })
            .await?;
        self.dream_stages_created.fetch_add(1, Ordering::Relaxed);
        let stage = self.repository.create_dream_stage("dream")?;
        // The TS `try` around the staged run (`service.ts:809-821`): any
        // failure first disposes the stage, then rethrows (a failing dispose
        // replaces the error, as a throw inside the `catch` block would).
        let staged: ServiceResult<(DreamExecutionResult, JsValue)> = async {
            let environment = ServiceDreamEnvironment { service: self };
            let logger = self.logger.clone().map(DreamLoggerAdapter);
            let mut runner =
                DreamRewriteRunner::new(&stage.repository, self.extractor.as_ref(), &environment);
            if let Some(logger) = &logger {
                runner = runner.with_logger(logger);
            }
            let outcome = runner.run(&JsString::from(trigger.as_str())).await?;
            let staged_snapshot = stage.repository.capture_current_memory_snapshot()?;
            stage.repository.close()?;
            Ok((outcome, staged_snapshot))
        }
        .await;
        let (outcome, staged_snapshot) = match staged {
            Ok(staged) => staged,
            Err(error) => {
                self.dream_stages_disposed.fetch_add(1, Ordering::Relaxed);
                return match stage.dispose() {
                    Ok(()) => Err(error),
                    Err(dispose_error) => Err(dispose_error.into()),
                };
            }
        };
        // `service.ts:821-824`: a falsy staged snapshot disposes the stage and
        // throws before any install work (the `outcome` half is structurally
        // impossible here — `run` returns a valid result or an error).
        if !js_truthy(&staged_snapshot) {
            self.dream_stages_disposed.fetch_add(1, Ordering::Relaxed);
            return match stage.dispose() {
                Ok(()) => Err(ServiceError::File(FileMemoryError::Invalid(
                    "Dream staging failed before a valid outcome was produced.".into(),
                ))),
                Err(dispose_error) => Err(dispose_error.into()),
            };
        }
        // TS wraps install/replace in `try { ... } finally { dispose }`
        // (`service.ts:826-857`): a dispose failure in `finally` overrides
        // the guarded error, exactly like a throw in the block.
        let install: ServiceResult<()> = if outcome.is_no_op {
            Ok(())
        } else {
            (|| {
                // `service.ts:838-847`: `after.runtimeState` is the dream
                // outcome, not the staged snapshot's; the version/counts
                // fields come from the staged snapshot.
                let mut after = JsObject::new();
                for key in ["workspaceVersion", "globalVersion", "counts"] {
                    after.insert(
                        key.into(),
                        staged_snapshot
                            .get(key)
                            .cloned()
                            .unwrap_or(JsValue::Undefined),
                    );
                }
                let mut runtime_state = JsObject::new();
                runtime_state.insert(
                    "lastDreamAt".into(),
                    JsValue::String(outcome.finished_at.clone()),
                );
                runtime_state.insert("lastDreamStatus".into(), JsValue::String("success".into()));
                runtime_state.insert(
                    "lastDreamSummary".into(),
                    JsValue::String(outcome.summary.clone()),
                );
                after.insert("runtimeState".into(), JsValue::Object(runtime_state));
                let metadata = build_last_dream_snapshot_metadata(
                    &self.workspace_dir,
                    &outcome.finished_at,
                    &stage.snapshot,
                    &JsValue::Object(after),
                    trigger,
                    outcome
                        .trace
                        .get("dreamTraceId")
                        .unwrap_or(&JsValue::Undefined),
                    &outcome.summary,
                );
                self.repository
                    .install_last_dream_snapshot(&stage.snapshot, &metadata)?;
                self.repository
                    .replace_live_roots_with_stage(&stage, &stage.snapshot)?;
                Ok(())
            })()
        };
        self.dream_stages_disposed.fetch_add(1, Ordering::Relaxed);
        match stage.dispose() {
            Ok(()) => install?,
            Err(dispose_error) => return Err(dispose_error.into()),
        }
        self.repository.storage.set_pipeline_state(
            LAST_DREAM_AT_STATE_KEY,
            &JsValue::String(outcome.finished_at.clone()),
        )?;
        self.repository.storage.set_pipeline_state(
            LAST_DREAM_STATUS_STATE_KEY,
            &JsValue::String("success".into()),
        )?;
        self.repository.storage.set_pipeline_state(
            LAST_DREAM_SUMMARY_STATE_KEY,
            &JsValue::String(outcome.summary.clone()),
        )?;
        self.repository
            .storage
            .save_dream_trace(&JsValue::Object(outcome.trace.clone()), DREAM_TRACE_LIMIT)?;
        self.reconcile_auto_dream_anchor(None, None)?;
        self.repository.get_file_memory_store().repair_manifests()?;
        self.retriever.reset_transient_state();
        Ok(DreamRunOutcome {
            prep_flush,
            reviewed_files: outcome.reviewed_files,
            rewritten_projects: outcome.rewritten_projects,
            deleted_projects: outcome.deleted_projects,
            deleted_files: outcome.deleted_files,
            profile_updated: outcome.profile_updated,
            duplicate_topic_count: outcome.duplicate_topic_count,
            conflict_topic_count: outcome.conflict_topic_count,
            summary: outcome.summary.clone(),
            trigger,
            status: "success",
        })
    }

    /// `rollbackLastDream` (`service.ts:881-887`), synchronous in TS.
    pub fn rollback_last_dream(&self) -> ServiceResult<JsValue> {
        let result = self.repository.rollback_last_dream_snapshot()?;
        let rolled_back_at = result
            .get("rolledBackAt")
            .and_then(JsValue::as_string)
            .cloned();
        self.reconcile_auto_dream_anchor(rolled_back_at.as_ref(), None)?;
        self.repository.get_file_memory_store().repair_manifests()?;
        self.retriever.reset_transient_state();
        Ok(result)
    }

    /// Test seam for the dream stage lifecycle (the TS fixture monkey-patches
    /// `createDreamStage` / `stage.dispose` to count them): `(created,
    /// disposed)` across this service instance so the replay can bind "a failed
    /// staged run disposes its stage exactly once".
    #[doc(hidden)]
    pub fn dream_stage_counters(&self) -> (u64, u64) {
        (
            self.dream_stages_created.load(Ordering::Relaxed),
            self.dream_stages_disposed.load(Ordering::Relaxed),
        )
    }

    /// `runDueScheduledMaintenance` (`service.ts:915-969`).
    pub async fn run_due_scheduled_maintenance(
        &self,
        reason: &str,
    ) -> ServiceResult<MaintenanceOutcome> {
        let settings = self.get_settings()?;
        let now_ms = (self.now_ms)() as f64;
        let mut overview = self.overview()?;
        let index_anchor_at = self.reconcile_auto_index_anchor(None, None)?;
        let pending_dialogue_turns = self.repository.storage.count_pending_dialogue_turns(None)?;
        let should_index_by_backlog =
            pending_dialogue_turns >= AUTO_INDEX_PENDING_DIALOGUE_TURN_THRESHOLD;
        let pending_sessions = match overview.get("pendingSessions") {
            Some(JsValue::Number(value)) => *value,
            _ => 0.0,
        };
        let should_index_by_interval = pending_sessions > 0.0
            && has_elapsed_minutes(
                index_anchor_at.as_ref(),
                settings.auto_index_interval_minutes,
                now_ms,
            )?;
        let mut index_stats = None;
        if should_index_by_backlog || should_index_by_interval {
            let scheduled_reason = if reason.starts_with("scheduled") {
                reason.to_string()
            } else {
                format!("scheduled:{reason}")
            };
            let flush_reason = if should_index_by_backlog {
                format!("{scheduled_reason}:pending_threshold")
            } else {
                scheduled_reason
            };
            let stats = self
                .flush(FlushOptions {
                    reason: Some(JsString::from(flush_reason)),
                    ..FlushOptions::default()
                })
                .await?;
            overview = self.overview()?;
            index_stats = Some(stats);
        }
        let mut file_input = JsObject::new();
        if let Some(JsValue::String(last_dream_at)) = overview.get("lastDreamAt") {
            file_input.insert("lastDreamAt".into(), JsValue::String(last_dream_at.clone()));
        }
        let file_overview = self
            .repository
            .get_file_memory_store()
            .get_overview(&file_input)?;
        let changed_files_since_last_dream = match file_overview.get("changedFilesSinceLastDream") {
            Some(JsValue::Number(value)) => *value,
            _ => 0.0,
        };
        let dream_anchor_at = self.reconcile_auto_dream_anchor(None, None)?;
        let mut dream_result = None;
        if changed_files_since_last_dream > 0.0
            && has_elapsed_minutes(
                dream_anchor_at.as_ref(),
                settings.auto_dream_interval_minutes,
                now_ms,
            )?
        {
            dream_result = Some(self.dream(DreamTrigger::Scheduled).await?);
        }
        Ok(MaintenanceOutcome {
            index_ran: index_stats.is_some(),
            dream_ran: dream_result.is_some(),
            index_stats,
            dream_result,
        })
    }
}
