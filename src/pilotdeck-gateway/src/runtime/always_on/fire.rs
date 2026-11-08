//! Port of TS `src/always-on/runtime/DiscoveryFire.ts` — the halves the
//! scheduler and the control surface touch: the five session-key
//! derivations, the `wx` discovery lock (`acquireDiscoveryLock` /
//! `releaseDiscoveryLock`) and the three entry points (`run`, `rerunPlan`,
//! `runApplyPhase`).
//!
//! The deep pipeline behind them — the five-phase discovery run, the
//! workspace strategies, the report tools, the model turns — is deliberately
//! NOT ported; this delivery is fail-closed at the seam. `run` answers an
//! `internal` AlwaysOnError ("not implemented in this embedding"), and the
//! two control entries keep only their real pre-seam checks:
//! `rerunPlan`'s `plan_not_found` / `plan_body_missing` (DiscoveryFire.ts:
//! 339-361) and `runApplyPhase`'s workspace read. The workspace-less cycle
//! answers the standalone-fallback envelope (`missing_workspace`,
//! createApplyHandler.ts:93) instead of the TS runtime path's TypeError —
//! reading `.strategy` of `undefined` is a JS engine artifact — which is the
//! one documented behavioral deviation of this seam
//! (`A7.8-ALWAYS-ON-BACKGROUND-001-R1.md`).
use std::{future::Future, io, pin::Pin, sync::Arc};

use pilotdeck_protocol::{JsObject, JsString, JsValue};

use crate::runtime::cron::stringify_pretty;

use super::{
    AlwaysOnError, AlwaysOnPaths, DiscoveryPlanRecord, DiscoveryPlanStore, WorkCycleRecord,
};

/// `DiscoveryFire.deriveDiscoverySessionKey` (DiscoveryFire.ts:244-246) and
/// its four siblings: `always-on/<phase>:project=<key>:run=<runId>`.
pub(crate) fn derive_discovery_session_key(project_key: &str, run_id: &str) -> String {
    format!("always-on/discovery:project={project_key}:run={run_id}")
}

pub(crate) fn derive_workspace_session_key(project_key: &str, run_id: &str) -> String {
    format!("always-on/workspace:project={project_key}:run={run_id}")
}

pub(crate) fn derive_execution_session_key(project_key: &str, run_id: &str) -> String {
    format!("always-on/execute:project={project_key}:run={run_id}")
}

pub(crate) fn derive_report_session_key(project_key: &str, run_id: &str) -> String {
    format!("always-on/report:project={project_key}:run={run_id}")
}

pub(crate) fn derive_apply_session_key(project_key: &str, run_id: &str) -> String {
    format!("always-on/apply:project={project_key}:run={run_id}")
}

/// `acquireDiscoveryLock` (DiscoveryFire.ts:1145-1159): `mkdir -p`, then a
/// `wx` (fail-if-exists) write of the pretty-printed payload — key order
/// `pid`, `runId`, `startedAt`, two-space indent, no trailing newline
/// (golden `l1`). EEXIST reads as `false`; other errors propagate.
pub(crate) fn acquire_discovery_lock(
    paths: &AlwaysOnPaths,
    pid: u32,
    run_id: &str,
    started_at: &str,
) -> io::Result<bool> {
    let mut payload = JsObject::new();
    payload.insert("pid".into(), JsValue::Number(f64::from(pid)));
    payload.insert("runId".into(), JsValue::String(JsString::from(run_id)));
    payload.insert(
        "startedAt".into(),
        JsValue::String(JsString::from(started_at)),
    );
    let bytes = stringify_pretty(&JsValue::Object(payload));
    let root = super::storage::root_for(&paths.state_file);
    super::storage::create_new(&root, &paths.discovery_lock_file, bytes.as_bytes())
}

/// `releaseDiscoveryLock` (DiscoveryFire.ts:1161-1163): the unlink swallows
/// every error, a missing lock included.
pub(crate) fn release_discovery_lock(paths: &AlwaysOnPaths) {
    let root = super::storage::root_for(&paths.state_file);
    let _ = super::storage::remove(&root, &paths.discovery_lock_file);
}

/// The `DiscoveryFireResult` slice the scheduler reads: `outcome`, which
/// re-arms the dormancy watcher when the run produced no plan
/// (DiscoveryScheduler.ts:154-156).
#[derive(Debug)]
pub(crate) struct FireOutcome {
    pub outcome: String,
}

/// `run` input — the scheduler's `{ runId, startedAt }` pair.
pub(crate) struct FireRunInput<'a> {
    pub run_id: &'a str,
    /// The timestamp the fire half's records stamp; the read side lands
    /// with the explicitly unported fire pipeline.
    #[allow(dead_code)]
    pub started_at_ms: f64,
}

/// `rerunPlan` input (DiscoveryFire.ts:332-336).
pub(crate) struct RerunFireInput<'a> {
    pub plan_id: &'a str,
    pub run_id: &'a str,
    /// The timestamp the fire half's records stamp; the read side lands
    /// with the explicitly unported fire pipeline.
    #[allow(dead_code)]
    pub started_at_ms: f64,
}

/// `runApplyPhase` input (DiscoveryFire.ts:264-270): the cycle, its
/// `{ id, title }` plan projection and the apply context.
pub(crate) struct ApplyPhaseInput<'a> {
    /// Apply-phase fields the fire pipeline consumes; the read side lands
    /// with the explicitly unported fire half (this PR's seam returns the
    /// documented `internal` error before touching them).
    #[allow(dead_code)]
    pub run_id: &'a str,
    pub cycle: &'a WorkCycleRecord,
    #[allow(dead_code)]
    pub plans: &'a [DiscoveryPlanRecord],
    #[allow(dead_code)]
    pub project_name: Option<&'a JsString>,
    #[allow(dead_code)]
    pub project_root: &'a str,
}

/// `rerunPlan`'s resolved result (the runtime echoes `runId` and `error`).
pub(crate) struct RerunFireResult {
    pub run_id: String,
    pub error: Option<super::manager::ControlError>,
}

/// `runApplyPhase`'s resolved result.
pub(crate) struct ApplyPhaseResult {
    pub session_key: String,
    pub error: Option<super::manager::ControlError>,
}

/// The fire seam as the scheduler and the control surface see it. The
/// futures are boxed so a tick can hold one across an await — which is what
/// makes `stop` wait for an in-flight fire instead of aborting it — and so
/// test fakes can block on their own schedule.
pub(crate) trait DiscoveryFirePort: Send + Sync {
    fn run<'a>(
        &'a self,
        input: FireRunInput<'a>,
    ) -> Pin<Box<dyn Future<Output = Result<FireOutcome, AlwaysOnError>> + Send + 'a>>;

    fn rerun_plan<'a>(
        &'a self,
        input: RerunFireInput<'a>,
    ) -> Pin<Box<dyn Future<Output = io::Result<RerunFireResult>> + Send + 'a>>;

    fn run_apply_phase<'a>(
        &'a self,
        input: ApplyPhaseInput<'a>,
    ) -> Pin<Box<dyn Future<Output = io::Result<ApplyPhaseResult>> + Send + 'a>>;
}

/// The fail-closed seam. Everything real before the deep pipeline — the
/// plan lookups, the markdown read, the `updateStatus({status: "ready"})`
/// write — runs; the pipeline itself answers `internal`.
pub(crate) struct DiscoveryFire {
    plan_store: Arc<DiscoveryPlanStore>,
}

impl DiscoveryFire {
    pub(crate) fn new(plan_store: Arc<DiscoveryPlanStore>) -> Self {
        Self { plan_store }
    }
}

impl DiscoveryFirePort for DiscoveryFire {
    fn run<'a>(
        &'a self,
        _input: FireRunInput<'a>,
    ) -> Pin<Box<dyn Future<Output = Result<FireOutcome, AlwaysOnError>> + Send + 'a>> {
        Box::pin(async {
            Err(AlwaysOnError::new(
                "internal",
                "not implemented in this embedding",
            ))
        })
    }

    fn rerun_plan<'a>(
        &'a self,
        input: RerunFireInput<'a>,
    ) -> Pin<Box<dyn Future<Output = io::Result<RerunFireResult>> + Send + 'a>> {
        Box::pin(async move {
            // DiscoveryFire.ts:339-349.
            if self.plan_store.get_record(input.plan_id)?.is_none() {
                return Ok(RerunFireResult {
                    run_id: input.run_id.into(),
                    error: Some(super::manager::ControlError {
                        code: "plan_not_found".into(),
                        message: format!("Plan {} not found", input.plan_id),
                    }),
                });
            }
            // DiscoveryFire.ts:351-361.
            if self.plan_store.read_plan_markdown(input.plan_id)?.is_none() {
                return Ok(RerunFireResult {
                    run_id: input.run_id.into(),
                    error: Some(super::manager::ControlError {
                        code: "plan_body_missing".into(),
                        message: format!("Plan markdown for {} not found on disk", input.plan_id),
                    }),
                });
            }
            // DiscoveryFire.ts:363 — the one pre-seam write.
            self.plan_store.update_status(input.plan_id, "ready")?;
            Ok(RerunFireResult {
                run_id: input.run_id.into(),
                error: Some(super::manager::ControlError {
                    code: "internal".into(),
                    message: "not implemented in this embedding".into(),
                }),
            })
        })
    }

    fn run_apply_phase<'a>(
        &'a self,
        input: ApplyPhaseInput<'a>,
    ) -> Pin<Box<dyn Future<Output = io::Result<ApplyPhaseResult>> + Send + 'a>> {
        Box::pin(async move {
            // The TS runtime reads `cycle.workspace.strategy` first
            // (DiscoveryFire.ts:273-277), so a workspace-less cycle is the
            // missing-workspace branch; this port answers the
            // standalone-fallback envelope instead of the engine TypeError.
            if input.cycle.workspace.is_none() {
                return Ok(ApplyPhaseResult {
                    session_key: String::new(),
                    error: Some(super::manager::ControlError {
                        code: "missing_workspace".into(),
                        message: "Cycle has no associated workspace to apply".into(),
                    }),
                });
            }
            Ok(ApplyPhaseResult {
                session_key: String::new(),
                error: Some(super::manager::ControlError {
                    code: "internal".into(),
                    message: "not implemented in this embedding".into(),
                }),
            })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::negative_assert_eq;
    use std::path::{Path, PathBuf};

    const GOLDEN: &str = include_str!("../../../tests/fixtures/always-on-2294/oracle.json");

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
        let home = std::env::temp_dir().join(format!("always-on-2294-fire-{tag}"));
        let _ = std::fs::remove_dir_all(&home);
        home
    }

    fn paths(home: &Path) -> AlwaysOnPaths {
        AlwaysOnPaths {
            project_key: "/proj/a".into(),
            state_file: home.join("state.json"),
            plans_dir: home.join("plans"),
            plan_index_file: home.join("plans").join("index.json"),
            cycle_index_file: home.join("cycles").join("index.json"),
            reports_dir: home.join("reports"),
            discovery_lock_file: home.join("locks").join("discovery.lock"),
        }
    }

    // Golden `l1`: the wx lock writes exactly these bytes, a second acquire
    // while held fails, and release is an unlink that tolerates repeats.
    #[test]
    fn replays_the_lock_golden() {
        let home = scratch("l1");
        let paths = paths(&home);
        let first = acquire_discovery_lock(&paths, 4242, "lock-run-1", "2026-01-01T00:05:00.000Z")
            .expect("#2294 first acquire");
        let bytes = std::fs::read_to_string(&paths.discovery_lock_file).expect("#2294 read");
        let second = acquire_discovery_lock(&paths, 99, "other", "x").expect("#2294 second");
        let bytes_after =
            std::fs::read_to_string(&paths.discovery_lock_file).expect("#2294 read after");
        release_discovery_lock(&paths);
        let after_release = paths.discovery_lock_file.exists();
        release_discovery_lock(&paths);
        negative_assert_eq!(
            serde_json::json!({
                "first": first,
                "bytes": bytes,
                "second": second,
                "bytesAfter": bytes_after,
                "afterRelease": after_release,
                "doubleReleaseOk": true,
            }),
            golden("l1-acquire-release"),
            "#2294 the lock contract matches the TS baseline"
        );
    }

    // The five derivations (DiscoveryFire.ts:244-262), pinned against the
    // session keys the r1 golden carries.
    #[test]
    fn derives_the_five_session_keys() {
        negative_assert_eq!(
            derive_discovery_session_key("x", "y"),
            "always-on/discovery:project=x:run=y",
            "#2294 discovery key"
        );
        negative_assert_eq!(
            derive_workspace_session_key("x", "y"),
            "always-on/workspace:project=x:run=y",
            "#2294 workspace key"
        );
        negative_assert_eq!(
            derive_execution_session_key("x", "y"),
            "always-on/execute:project=x:run=y",
            "#2294 execution key"
        );
        negative_assert_eq!(
            derive_report_session_key("x", "y"),
            "always-on/report:project=x:run=y",
            "#2294 report key"
        );
        negative_assert_eq!(
            derive_apply_session_key("x", "y"),
            "always-on/apply:project=x:run=y",
            "#2294 apply key"
        );
    }

    fn plan_fixture(home: &Path) {
        let index = home.join("plans").join("index.json");
        std::fs::create_dir_all(index.parent().expect("#2294 parent")).expect("#2294 mkdir");
        std::fs::write(
            &index,
            "{\n  \"schemaVersion\": 1,\n  \"plans\": [\n    {\n      \"id\": \"p1\",\n      \
             \"title\": \"T\",\n      \"createdAt\": \"2026-01-01T00:00:00.000Z\",\n      \
             \"status\": \"ready\",\n      \"summary\": \"s\",\n      \"rationale\": \"r\",\n      \
             \"dedupeKey\": \"d\",\n      \"sourceRunId\": \"r0\",\n      \"planFilePath\": \
             \"plans/p1.md\"\n    }\n  ]\n}",
        )
        .expect("#2294 plan index");
    }

    fn as_error_json(error: Option<super::super::manager::ControlError>) -> serde_json::Value {
        match error {
            Some(error) => serde_json::json!({"code": error.code, "message": error.message}),
            None => serde_json::Value::Null,
        }
    }

    // The rerun seam keeps its two real checks byte-identical to the r1
    // golden records and fail-closes past them.
    #[tokio::test]
    async fn rerun_seam_keeps_the_real_checks_then_fail_closes() {
        let home = scratch("r1-rerun");
        let paths = paths(&home);
        plan_fixture(&home);
        let fire = DiscoveryFire::new(Arc::new(DiscoveryPlanStore::new(paths.clone())));

        // r1 `rerunMissing`: plan_not_found with the runId echo.
        let missing = fire
            .rerun_plan(RerunFireInput {
                plan_id: "p-missing",
                run_id: "probe-run-7",
                started_at_ms: 0.0,
            })
            .await
            .expect("#2294 rerun");
        negative_assert_eq!(
            serde_json::json!({"runId": missing.run_id, "error": as_error_json(missing.error)}),
            golden("r1-control-envelopes")["rerunMissing"],
            "#2294 plan_not_found envelope"
        );

        // r1 `rerunNoBody`: the record exists, the markdown does not.
        let no_body = fire
            .rerun_plan(RerunFireInput {
                plan_id: "p1",
                run_id: "probe-run-8",
                started_at_ms: 0.0,
            })
            .await
            .expect("#2294 rerun");
        negative_assert_eq!(
            serde_json::json!({"runId": no_body.run_id, "error": as_error_json(no_body.error)}),
            golden("r1-control-envelopes")["rerunNoBody"],
            "#2294 plan_body_missing envelope"
        );

        // Past the checks: the pre-seam `updateStatus({status: "ready"})`
        // write lands, then the seam fail-closes.
        std::fs::create_dir_all(&paths.plans_dir).expect("#2294 mkdir");
        std::fs::write(paths.plans_dir.join("p1.md"), "# plan").expect("#2294 markdown");
        let seam = fire
            .rerun_plan(RerunFireInput {
                plan_id: "p1",
                run_id: "probe-run-9",
                started_at_ms: 0.0,
            })
            .await
            .expect("#2294 rerun");
        negative_assert_eq!(
            as_error_json(seam.error),
            serde_json::json!({
                "code": "internal",
                "message": "not implemented in this embedding"
            }),
            "#2294 the deep pipeline fail-closes"
        );
        let index = std::fs::read_to_string(&paths.plan_index_file).expect("#2294 index");
        negative_assert_eq!(
            serde_json::from_str::<serde_json::Value>(&index).expect("#2294 parse")["plans"][0]["status"],
            "ready",
            "#2294 the pre-seam status write ran"
        );
    }

    // The apply seam: a workspace-less cycle answers the standalone-fallback
    // `missing_workspace` envelope (the documented deviation from the TS
    // runtime path's TypeError); a workspace-bearing cycle fail-closes.
    #[tokio::test]
    async fn apply_seam_answers_missing_workspace_then_fail_closes() {
        let home = scratch("r1-apply");
        let paths = paths(&home);
        let fire = DiscoveryFire::new(Arc::new(DiscoveryPlanStore::new(paths)));
        let workspaceless = WorkCycleRecord {
            raw: JsObject::new(),
            id: "c1".into(),
            project_key: "/proj/a".into(),
            status: "active".into(),
            workspace: None,
            plan_ids: Vec::new(),
            created_at: "2026-01-01T00:00:00.000Z".into(),
            created_by_run_id: "r0".into(),
        };
        let result = fire
            .run_apply_phase(ApplyPhaseInput {
                run_id: "run-1",
                cycle: &workspaceless,
                plans: &[],
                project_name: Some(&"P".into()),
                project_root: "/proj/a",
            })
            .await
            .expect("#2294 apply");
        negative_assert_eq!(
            serde_json::json!({
                "sessionKey": result.session_key,
                "error": as_error_json(result.error),
            }),
            serde_json::json!({
                "sessionKey": "",
                "error": {
                    "code": "missing_workspace",
                    "message": "Cycle has no associated workspace to apply"
                },
            }),
            "#2294 the workspace-less apply envelope"
        );
        let workspace = super::super::cycles::WorkCycleWorkspace {
            strategy: "snapshot-copy".into(),
            cwd: "/w".into(),
            metadata: JsObject::new(),
        };
        let cycle = WorkCycleRecord {
            workspace: Some(workspace),
            ..workspaceless
        };
        let result = fire
            .run_apply_phase(ApplyPhaseInput {
                run_id: "run-2",
                cycle: &cycle,
                plans: &[],
                project_name: Some(&"P".into()),
                project_root: "/proj/a",
            })
            .await
            .expect("#2294 apply");
        negative_assert_eq!(
            as_error_json(result.error),
            serde_json::json!({
                "code": "internal",
                "message": "not implemented in this embedding"
            }),
            "#2294 the workspace-bearing apply fail-closes"
        );
    }

    // `run` is the pure fail-closed surface: an internal AlwaysOnError whose
    // code the scheduler preserves on rethrow (golden `t11` shape).
    #[tokio::test]
    async fn run_seam_fail_closes() {
        let home = scratch("run-seam");
        let fire = DiscoveryFire::new(Arc::new(DiscoveryPlanStore::new(paths(&home))));
        let error = fire
            .run(FireRunInput {
                run_id: "run-1",
                started_at_ms: 0.0,
            })
            .await
            .expect_err("#2294 the deep pipeline is not implemented");
        negative_assert_eq!(error.code(), "internal", "#2294 code");
        negative_assert_eq!(
            error.message().to_utf8().expect("#2294 UTF-8"),
            "not implemented in this embedding",
            "#2294 message"
        );
    }
}
