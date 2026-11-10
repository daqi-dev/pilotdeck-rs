//! S1 unit tests for the #1325 CronManager port: the runtimes map, the
//! start/stop lifecycle, and the injected `SessionAdmission` turn port. The
//! six-method gold replay lives with the S5 fixtures; these cover the
//! lifecycle matrix cells that need no oracle (16/17/18 plus the
//! duplicate-submit cell 11 and the stop-drain seam).
use std::{
    fs,
    path::Path,
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicUsize, Ordering},
    },
};

use pilotdeck_protocol::{JsObject, JsString, JsValue, parse_json, stringify_json};

use super::{
    CronAgentGateway, CronError, CronGatewayBody, CronLogger, CronManager, CronManagerConfig,
    CronManagerOptions, CronTurnSubmit, SessionAdmissionCronGateway, dispatch, methods,
    resolve_cron_paths,
};
use crate::{runtime::session::SessionAdmission, test_scratch::Scratch};

/// `installFixedClock()`'s default epoch (`Date.UTC(2026, 0, 1)`), the pin the
/// #1216 store oracle already uses.
const PINNED_NOW_MS: f64 = 1_767_225_600_000.0;

fn json(raw: &str) -> JsValue {
    parse_json(raw).unwrap_or_else(|error| panic!("test fixture JSON must parse: {error:?}"))
}

fn text(value: &JsValue, field: &str) -> String {
    value
        .get(field)
        .and_then(JsValue::as_string)
        .unwrap_or_else(|| panic!("test field {field} must be a string"))
        .to_utf8()
        .expect("test fixture strings are UTF-8")
}

/// The runtime logger, recording `level message meta` lines.
#[derive(Default)]
struct RecordingLogger {
    messages: Mutex<Vec<String>>,
}
impl CronLogger for RecordingLogger {
    fn info(&self, message: &str, meta: &JsObject) {
        self.record("info", message, meta);
    }
    fn warn(&self, message: &str, meta: &JsObject) {
        self.record("warn", message, meta);
    }
}
impl RecordingLogger {
    fn record(&self, level: &str, message: &str, meta: &JsObject) {
        let meta = stringify_json(&JsValue::Object(meta.clone()))
            .unwrap_or_default()
            .unwrap_or_else(|| "null".to_owned());
        self.messages
            .lock()
            .unwrap()
            .push(format!("{level} {message} {meta}"));
    }
    fn messages(&self) -> Vec<String> {
        self.messages.lock().unwrap().clone()
    }
}

/// The `CronGatewayBody` half behind the admission port, recording every
/// facade call.
#[derive(Default)]
struct RecordingBody {
    calls: Mutex<Vec<String>>,
}
impl CronGatewayBody for RecordingBody {
    fn run_turn(&self, input: &CronTurnSubmit) -> Result<(), CronError> {
        self.record(format!(
            "run_turn {} {}",
            input.session_key.to_utf8().unwrap(),
            input.run_id.to_utf8().unwrap(),
        ));
        Ok(())
    }
    fn abort_turn(
        &self,
        session_key: &JsString,
        run_id: Option<&JsString>,
        reason: Option<&JsString>,
    ) -> Result<(), CronError> {
        self.record(format!(
            "abort_turn {} {} {}",
            session_key.to_utf8().unwrap(),
            run_id
                .map(|id| id.to_utf8().unwrap())
                .as_deref()
                .unwrap_or("-"),
            reason
                .map(|id| id.to_utf8().unwrap())
                .as_deref()
                .unwrap_or("-"),
        ));
        Ok(())
    }
    fn close_session(
        &self,
        session_key: &JsString,
        reason: Option<&JsString>,
    ) -> Result<(), CronError> {
        self.record(format!(
            "close_session {} {}",
            session_key.to_utf8().unwrap(),
            reason
                .map(|id| id.to_utf8().unwrap())
                .as_deref()
                .unwrap_or("-"),
        ));
        Ok(())
    }
}
impl RecordingBody {
    fn record(&self, line: String) {
        self.calls.lock().unwrap().push(line);
    }
    fn calls(&self) -> Vec<String> {
        self.calls.lock().unwrap().clone()
    }
}

/// One manager over its own scratch home, with pinned clock and counter
/// taskIds, and (when started) the admission port over a recording body.
struct Harness {
    home: std::path::PathBuf,
    manager: Arc<CronManager>,
    logger: Arc<RecordingLogger>,
    body: Option<Arc<RecordingBody>>,
    /// Dropped last, so the scratch tree outlives the manager's file writes.
    _scratch: Scratch,
}

fn harness(started: bool) -> Harness {
    let scratch = Scratch::new("cron-manager-s1");
    let home = scratch.path().join("home");
    fs::create_dir_all(&home).unwrap();
    let logger = Arc::new(RecordingLogger::default());
    let next_task = Arc::new(AtomicUsize::new(0));
    let manager = CronManager::new(CronManagerOptions {
        config: CronManagerConfig::default(),
        pilot_home: home.to_string_lossy().into_owned(),
        session_overrides: None,
        now: Some(Arc::new(|| PINNED_NOW_MS)),
        uuid: Some(Arc::new(move || {
            JsString::from(format!(
                "task-{}",
                next_task.fetch_add(1, Ordering::SeqCst) + 1
            ))
        })),
        logger: Some(logger.clone()),
        scheduler_idle_poll_ms: None,
    })
    .unwrap();
    let mut harness = Harness {
        home,
        manager,
        logger,
        body: None,
        _scratch: scratch,
    };
    if started {
        let body = Arc::new(RecordingBody::default());
        let gateway: Arc<dyn CronAgentGateway> = Arc::new(SessionAdmissionCronGateway::new(
            Arc::new(SessionAdmission::default()),
            body.clone(),
        ));
        harness.manager.bind_agent_gateway(gateway).unwrap();
        harness.manager.start().unwrap();
        harness.body = Some(body);
    }
    harness
}

/// Write a project's `tasks.json` plus the `.cwd` marker, exactly where
/// `resolve_cron_paths` puts them (the same resolution discovery uses).
fn seed_project(home: &Path, project_key: &str, tasks: &str) {
    let paths = resolve_cron_paths(&home.to_string_lossy(), project_key).unwrap();
    fs::create_dir_all(&paths.project_dir).unwrap();
    fs::write(&paths.tasks_file, tasks).unwrap();
    fs::write(paths.project_dir.join(".cwd"), format!("{project_key}\n")).unwrap();
}

fn read_tasks(home: &Path, project_key: &str) -> Vec<JsValue> {
    let paths = resolve_cron_paths(&home.to_string_lossy(), project_key).unwrap();
    let raw = fs::read_to_string(&paths.tasks_file).unwrap();
    match parse_json(&raw) {
        Ok(value) => match value.get("tasks") {
            Some(JsValue::Array(tasks)) => tasks.clone(),
            _ => Vec::new(),
        },
        Err(_) => Vec::new(),
    }
}

fn run_history(home: &Path, project_key: &str) -> Vec<JsValue> {
    let paths = resolve_cron_paths(&home.to_string_lossy(), project_key).unwrap();
    fs::read_to_string(&paths.run_history_file)
        .unwrap_or_default()
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| parse_json(line).expect("run history lines parse"))
        .collect()
}

fn create_input(project_key: &str) -> JsValue {
    json(&format!(
        r#"{{
            "projectKey": "{project_key}",
            "message": "m1",
            "sessionKey": "origin-session",
            "channelKey": "origin-channel",
            "mode": "default",
            "schedule": {{"type": "once", "runAt": "2027-01-01T00:00:00.000Z"}}
        }}"#
    ))
}

/// A running cron task whose `lastRunId` has no terminal run record — the
/// restart-recovery shape.
fn seeded_running_task() -> String {
    r#"{
        "schemaVersion": 1,
        "tasks": [{
            "schemaVersion": 1,
            "taskId": "task-seeded",
            "message": "seeded",
            "schedule": {"type": "cron", "expression": "5 * * * *", "timezone": "UTC"},
            "status": "running",
            "sessionKey": "cron:task-seeded",
            "channelKey": "cron",
            "originSessionKey": "origin-session",
            "originChannelKey": "origin-channel",
            "projectKey": "/proj",
            "timezone": "UTC",
            "createdAt": "2025-12-31T00:00:00.000Z",
            "updatedAt": "2025-12-31T00:00:00.000Z",
            "nextRunAt": "2026-01-01T00:05:00.000Z",
            "lastRunId": "run-old",
            "revision": 0,
            "scheduleComputationVersion": 3
        }]
    }"#
    .to_owned()
}

// Matrix cell 16: `ensureRuntime` from a create on an unstarted manager never
// starts the runtime, so a seeded running task stays running (no recovery,
// no run-history record).
#[test]
fn unstarted_manager_creates_without_recovery() {
    let harness = harness(false);
    seed_project(&harness.home, "/proj", &seeded_running_task());

    let created = harness.manager.create_task(&create_input("/proj")).unwrap();
    let task = created.get("task").unwrap();
    assert_eq!(text(task, "taskId"), "task-1");
    assert_eq!(text(task, "sessionKey"), "cron:task-1");
    assert_eq!(text(task, "status"), "scheduled");
    assert_eq!(
        text(task, "nextRunAt"),
        "2027-01-01T00:00:00.000Z",
        "the once runAt survives as the persisted nextRunAt"
    );

    let tasks = read_tasks(&harness.home, "/proj");
    let seeded = tasks
        .iter()
        .find(|task| text(task, "taskId") == "task-seeded")
        .expect("seeded task still on disk");
    assert_eq!(text(seeded, "status"), "running");
    assert!(
        run_history(&harness.home, "/proj").is_empty(),
        "no interrupted-run record without a started runtime"
    );
    assert!(
        !harness
            .logger
            .messages()
            .iter()
            .any(|line| line.contains("cron runtime started")),
        "the runtime never started"
    );
}

// #2181: two creates in the same zone resolve one TZif parse — every
// runtime the manager loads shares its zone cache, and
// `compute_next_run_at`'s call point takes it; a different zone bumps the
// count back apart, so the pin is reuse, not "never parse".
#[test]
fn create_task_reuses_the_zone_resolver_cache() {
    let harness = harness(false);

    let create = |timezone: &str| {
        harness
            .manager
            .create_task(&json(&format!(
                r#"{{
                    "projectKey": "/proj",
                    "message": "m1",
                    "schedule": {{"type": "cron", "expression": "5 * * * *", "timezone": "{timezone}"}}
                }}"#
            )))
            .unwrap();
    };
    create("Asia/Tokyo");
    create("Asia/Tokyo");
    assert_eq!(
        harness.manager.zone_resolver_cache().parse_count(),
        1,
        "#2181 zone reuse: a second create in the same zone must not re-parse the TZif file"
    );
    create("Europe/Berlin");
    assert_eq!(
        harness.manager.zone_resolver_cache().parse_count(),
        2,
        "#2181 zone reuse: a new zone resolves exactly once more"
    );
}

// Matrix cell 17: after `stop`, a create through the same manager loads a
// runtime that is not started again.
#[test]
fn stopped_manager_create_does_not_restart_runtimes() {
    let harness = harness(true);
    harness.manager.stop().unwrap();
    seed_project(&harness.home, "/proj", &seeded_running_task());

    harness.manager.create_task(&create_input("/proj")).unwrap();

    let tasks = read_tasks(&harness.home, "/proj");
    let seeded = tasks
        .iter()
        .find(|task| text(task, "taskId") == "task-seeded")
        .expect("seeded task still on disk");
    assert_eq!(text(seeded, "status"), "running");
    assert!(run_history(&harness.home, "/proj").is_empty());
}

// #2494: the shared stop latch re-arms with a new lifecycle. No production
// path restarts a stopped manager (the reload ladder stages fresh ones via
// the factories), but the latch flips in the same place `started` does, so
// a stopped-then-started manager must remain stoppable — that second stop
// is then the only one a later caller observes.
#[test]
fn a_restarted_manager_stops_again() {
    let harness = harness(false);
    let body = Arc::new(RecordingBody::default());
    let gateway: Arc<dyn CronAgentGateway> = Arc::new(SessionAdmissionCronGateway::new(
        Arc::new(SessionAdmission::default()),
        body.clone(),
    ));
    harness.manager.bind_agent_gateway(gateway).unwrap();
    seed_project(&harness.home, "/proj", &seeded_running_task());
    harness.manager.start().unwrap();
    assert_eq!(
        harness.manager.runtime_count(),
        1,
        "#2494 the started manager discovered the seeded project"
    );
    harness.manager.stop().unwrap();
    assert_eq!(
        harness.manager.runtime_count(),
        0,
        "#2494 the first stop cleared the runtimes"
    );
    // A second stop through any path observes the first.
    harness
        .manager
        .stop()
        .expect("#2494 the second stop observes the first");
    harness.manager.start().unwrap();
    assert_eq!(
        harness.manager.runtime_count(),
        1,
        "#2494 the restart re-discovers the project"
    );
    harness.manager.stop().unwrap();
    assert_eq!(
        harness.manager.runtime_count(),
        0,
        "#2494 the restarted manager stops again"
    );
}

// Matrix cell 18: none of the six management methods needs a bound gateway;
// only the two `start`s gate on it.
#[test]
fn methods_run_without_gateway_and_start_requires_bind() {
    let harness = harness(false);

    let created = harness.manager.create_task(&create_input("/proj")).unwrap();
    let task_id = text(created.get("task").unwrap(), "taskId");

    let listed = harness
        .manager
        .list_tasks(&json(r#"{"projectKey": "/proj", "includeHistory": true}"#))
        .unwrap();
    assert_eq!(
        match listed.get("tasks") {
            Some(JsValue::Array(tasks)) => tasks.len(),
            _ => panic!("tasks must be an array"),
        },
        1,
        "the created task is listed"
    );

    let updated = harness
        .manager
        .update_task(&json(&format!(
            r#"{{
                "taskId": "{task_id}",
                "projectKey": "/proj",
                "expectedRevision": 0,
                "message": "m2",
                "schedule": {{"type": "cron", "expression": "5 * * * *", "timezone": "UTC"}}
            }}"#
        )))
        .unwrap();
    assert_eq!(
        updated.get("updated").and_then(JsValue::as_bool),
        Some(true)
    );
    assert_eq!(
        updated
            .get("task")
            .and_then(|task| task.get("revision").and_then(JsValue::as_number)),
        Some(1.0)
    );

    let run_now = harness
        .manager
        .run_task_now(&json(&format!(r#"{{"taskId": "{task_id}"}}"#)))
        .unwrap();
    assert_eq!(
        run_now.get("started").and_then(JsValue::as_bool),
        Some(true)
    );

    let stopped = harness
        .manager
        .stop_task(&json(&format!(r#"{{"taskId": "{task_id}"}}"#)))
        .unwrap();
    assert_eq!(
        stopped.get("stopped").and_then(JsValue::as_bool),
        Some(false),
        "no active run, so the idle result"
    );

    let deleted = harness
        .manager
        .delete_task(&json(&format!(r#"{{"taskId": "{task_id}"}}"#)))
        .unwrap();
    assert_eq!(
        deleted.get("deleted").and_then(JsValue::as_bool),
        Some(true)
    );

    let error = harness.manager.start().unwrap_err();
    assert_eq!(
        error.message().to_utf8().unwrap(),
        "CronManager.start called before bindAgentGateway."
    );
    let error = harness
        .manager
        .loaded_runtime("/proj")
        .expect("the create loaded a runtime")
        .start()
        .unwrap_err();
    assert_eq!(
        error.message().to_utf8().unwrap(),
        "CronRuntime.start called before bindGateway."
    );
}

// #2047: `runTaskNow` builds its one-time schedule from one clock read and
// hands that same instant to `createTask`. TS reads the clock twice
// (`new Date()` there, `Date.now()` inside `createTask`), so a tick between
// the two reads makes the schedule it just built look stale and the run-now
// answers "One-time Cron tasks must be scheduled in the future.". A clock that
// advances on every read is exactly that tick, held still: the run-now must
// still start.
#[test]
fn run_now_shares_one_clock_read_with_the_task_it_creates() {
    let scratch = Scratch::new("cron-manager-2047");
    let home = scratch.path().join("home");
    fs::create_dir_all(&home).unwrap();
    let reads = Arc::new(AtomicUsize::new(0));
    let clock = reads.clone();
    let manager = CronManager::new(CronManagerOptions {
        config: CronManagerConfig::default(),
        pilot_home: home.to_string_lossy().into_owned(),
        session_overrides: None,
        now: Some(Arc::new(move || {
            PINNED_NOW_MS + clock.fetch_add(1, Ordering::SeqCst) as f64
        })),
        uuid: Some(Arc::new(move || JsString::from("task-run-now"))),
        logger: Some(Arc::new(RecordingLogger::default())),
        scheduler_idle_poll_ms: None,
    })
    .unwrap();
    let created = manager.create_task(&create_input("/proj")).unwrap();
    let task_id = text(created.get("task").unwrap(), "taskId");

    let run_now = manager
        .run_task_now(&json(&format!(r#"{{"taskId": "{task_id}"}}"#)))
        .expect("#2047 runTaskNow must not reject the schedule it just stamped");
    assert_eq!(
        run_now.get("started").and_then(JsValue::as_bool),
        Some(true),
        "#2047 a clock tick between runTaskNow's two TS reads must not expire its own schedule"
    );
    assert!(
        reads.load(Ordering::SeqCst) >= 2,
        "#2047 the test clock must have ticked, otherwise this case proves nothing"
    );
}

// Matrix cell 11: a duplicate submit for an admitted session is rejected by
// the same `SessionAdmission` fence — the second trigger never executes —
// and a sequential submit after the first turn ends is admitted again.
#[test]
fn duplicate_submit_rejected_by_same_admission() {
    struct ReentrantBody {
        gateway: OnceLock<std::sync::Weak<SessionAdmissionCronGateway>>,
        reentry: Mutex<Vec<Result<(), CronError>>>,
    }
    impl CronGatewayBody for ReentrantBody {
        fn run_turn(&self, input: &CronTurnSubmit) -> Result<(), CronError> {
            // Re-enter the port synchronously, mid-turn.
            let gateway = self.gateway.get().unwrap().upgrade().unwrap();
            let outcome = gateway.submit_turn(input);
            self.reentry.lock().unwrap().push(outcome);
            Ok(())
        }
        fn abort_turn(
            &self,
            _session_key: &JsString,
            _run_id: Option<&JsString>,
            _reason: Option<&JsString>,
        ) -> Result<(), CronError> {
            Ok(())
        }
        fn close_session(
            &self,
            _session_key: &JsString,
            _reason: Option<&JsString>,
        ) -> Result<(), CronError> {
            Ok(())
        }
    }

    let body = Arc::new(ReentrantBody {
        gateway: OnceLock::new(),
        reentry: Mutex::new(Vec::new()),
    });
    let admission = Arc::new(SessionAdmission::default());
    let gateway = Arc::new(SessionAdmissionCronGateway::new(
        admission.clone(),
        body.clone(),
    ));
    body.gateway.set(Arc::downgrade(&gateway)).unwrap();

    let submit = |run_id: &str| CronTurnSubmit {
        session_key: JsString::from("session-a"),
        channel_key: JsString::from("cron"),
        project_key: None,
        message: JsString::from("m"),
        mode: JsString::from("default"),
        run_id: JsString::from(run_id),
        timeout_ms: 0.0,
    };

    gateway.submit_turn(&submit("run-1")).unwrap();
    let reentry = body.reentry.lock().unwrap();
    let error = reentry[0]
        .as_ref()
        .expect_err("the duplicate submit is rejected by the same acceptor");
    assert_eq!(error.code(), Some("session_busy"));
    assert_eq!(
        error.message().to_utf8().unwrap(),
        "Session session-a already has an active turn."
    );
    drop(reentry);
    assert!(
        !admission.has_active_turn(&JsString::from("session-a")),
        "the outer turn's end_turn released the fence"
    );

    gateway.submit_turn(&submit("run-2")).unwrap();
    assert!(!admission.has_active_turn(&JsString::from("session-a")));
}

// The runtimes map is keyed by the resolved project key (CronManager.ts:187).
#[test]
fn runtimes_map_keys_by_resolved_project() {
    let harness = harness(false);
    harness
        .manager
        .create_task(&create_input("/alpha"))
        .unwrap();
    harness.manager.create_task(&create_input("/beta")).unwrap();
    assert_eq!(harness.manager.runtime_count(), 2);

    // Trailing separators and `..` segments fold to the same key.
    harness
        .manager
        .create_task(&create_input("/alpha/"))
        .unwrap();
    harness
        .manager
        .create_task(&create_input("/beta/../alpha/"))
        .unwrap();
    assert_eq!(harness.manager.runtime_count(), 2, "same resolved key");

    assert!(harness.manager.loaded_runtime("/alpha").is_some());
    assert!(harness.manager.loaded_runtime("/gamma").is_none());
}

// `start` discovers recorded project keys, starts their runtimes (recovery
// included), and `stop` aborts every active run with the shutdown reason and
// releases task sessions (matrix cells 10 and 13's recovery half).
#[test]
fn start_discovers_projects_and_stop_drains() {
    let harness = harness(false);
    seed_project(&harness.home, "/proj", &seeded_running_task());

    let body = Arc::new(RecordingBody::default());
    let admission = Arc::new(SessionAdmission::default());
    let gateway: Arc<dyn CronAgentGateway> =
        Arc::new(SessionAdmissionCronGateway::new(admission, body.clone()));
    harness.manager.bind_agent_gateway(gateway).unwrap();
    harness.manager.start().unwrap();

    assert_eq!(harness.manager.runtime_count(), 1);
    assert!(
        harness
            .logger
            .messages()
            .iter()
            .any(|line| line.starts_with("info cron runtime started"))
    );

    // recoverInterruptedRuns: the interrupted run is recorded as failed and
    // the task is rescheduled with revision+1.
    let tasks = read_tasks(&harness.home, "/proj");
    let seeded = tasks
        .iter()
        .find(|task| text(task, "taskId") == "task-seeded")
        .expect("recovered task on disk");
    assert_eq!(text(seeded, "status"), "scheduled");
    assert_eq!(
        seeded.get("revision").and_then(JsValue::as_number),
        Some(1.0)
    );
    let history = run_history(&harness.home, "/proj");
    assert_eq!(history.len(), 1);
    assert_eq!(text(&history[0], "runId"), "run-old");
    assert_eq!(text(&history[0], "outcome"), "failed");
    assert_eq!(
        text(history[0].get("error").unwrap(), "code"),
        "cron_run_interrupted"
    );
    assert!(
        body.calls()
            .contains(&"close_session cron:task-seeded cron/unattended-policy-refresh".to_owned())
    );

    // The CronFire seam: an in-flight run gets the shutdown abort and the
    // task session is released.
    let runtime = harness.manager.loaded_runtime("/proj").unwrap();
    runtime.register_active_run(super::CronActiveRun {
        run_id: JsString::from("run-active"),
        task_id: JsString::from("task-seeded"),
        session_key: JsString::from("cron:task-seeded"),
        schedule_type: JsString::from("cron"),
        stop_requested: false,
    });
    harness.manager.stop().unwrap();

    assert_eq!(harness.manager.runtime_count(), 0, "runtimes cleared");
    let calls = body.calls();
    assert!(
        calls.contains(&"abort_turn cron:task-seeded run-active system:cron_shutdown".to_owned()),
        "stop aborts the active run with the shutdown reason: {calls:?}"
    );
    assert_eq!(
        calls.last().unwrap(),
        "close_session cron:task-seeded cron/task-finished",
        "stop releases the task session"
    );
    // stop() marks and aborts but leaves the registry to the run's own
    // finally (TS relies on CronFire's cleanup, fire.rs).
    assert_eq!(runtime.active_run_count(), 1);
    assert!(
        runtime
            .active_run(&JsString::from("run-active"))
            .is_some_and(|run| run.stop_requested)
    );
}

// The manager's global list merges every runtime sorted by
// `(nextRunAt ?? "").localeCompare` (CronManager.ts:126-141).
#[test]
fn global_list_merges_and_sorts_by_next_run_at() {
    let harness = harness(false);
    // Scheduled tasks survive start() untouched, so the seeded nextRunAt
    // values are what the merge sorts on.
    let seed = |project_key: &str, tasks: &[(&str, Option<&str>)]| {
        let entries: Vec<String> = tasks
            .iter()
            .map(|(task_id, next_run_at)| {
                let next_run_at_line = next_run_at
                    .map(|value| format!("\"nextRunAt\": \"{value}\",\n"))
                    .unwrap_or_default();
                format!(
                    r#"{{
                "schemaVersion": 1,
                "taskId": "{task_id}",
                "message": "seeded",
                "schedule": {{"type": "cron", "expression": "5 * * * *", "timezone": "UTC"}},
                "status": "scheduled",
                "sessionKey": "cron:{task_id}",
                "channelKey": "cron",
                "originSessionKey": "origin-session",
                "originChannelKey": "origin-channel",
                "projectKey": "{project_key}",
                "timezone": "UTC",
                "createdAt": "2025-12-31T00:00:00.000Z",
                "updatedAt": "2025-12-31T00:00:00.000Z",
                {next_run_at_line}"revision": 0,
                "scheduleComputationVersion": 3
            }}"#
                )
            })
            .collect();
        let document = format!(
            "{{\"schemaVersion\": 1, \"tasks\": [{}]}}",
            entries.join(",")
        );
        seed_project(&harness.home, project_key, &document);
    };
    seed("/zeta", &[("task-late", Some("2026-03-01T00:00:00.000Z"))]);
    seed(
        "/alpha",
        &[
            ("task-early", Some("2026-02-01T00:00:00.000Z")),
            ("task-none", None),
        ],
    );

    let body = Arc::new(RecordingBody::default());
    let gateway: Arc<dyn CronAgentGateway> = Arc::new(SessionAdmissionCronGateway::new(
        Arc::new(SessionAdmission::default()),
        body.clone(),
    ));
    harness.manager.bind_agent_gateway(gateway).unwrap();
    harness.manager.start().unwrap();

    let listed = harness
        .manager
        .list_tasks(&json(r#"{"includeHistory": false}"#))
        .unwrap();
    let order: Vec<String> = match listed.get("tasks") {
        Some(JsValue::Array(tasks)) => tasks.iter().map(|task| text(task, "taskId")).collect(),
        _ => panic!("tasks must be an array"),
    };
    assert_eq!(
        order,
        [
            "task-none".to_owned(),
            "task-early".to_owned(),
            "task-late".to_owned()
        ],
        "the nullish nextRunAt folds to \"\", which sorts before every stamp"
    );
}

// `listTasks` applies `slice(0, limit ?? 50)` to the *merged* run page too
// (CronManager.ts:121-142): without a limit the global list must stop at 50,
// not at 50 per runtime. Two runtimes with 30 runs each merge to 60.
#[test]
fn global_list_caps_the_merged_run_page_at_the_nullish_default() {
    let harness = harness(false);
    let seed = |project_key: &str, task_id: &str| {
        let document = format!(
            r#"{{
                "schemaVersion": 1,
                "tasks": [{{
                    "schemaVersion": 1,
                    "taskId": "{task_id}",
                    "message": "seeded",
                    "schedule": {{"type": "cron", "expression": "5 * * * *", "timezone": "UTC"}},
                    "status": "scheduled",
                    "sessionKey": "cron:{task_id}",
                    "channelKey": "cron",
                    "originSessionKey": "origin-session",
                    "originChannelKey": "origin-channel",
                    "projectKey": "{project_key}",
                    "timezone": "UTC",
                    "createdAt": "2025-12-31T00:00:00.000Z",
                    "updatedAt": "2025-12-31T00:00:00.000Z",
                    "nextRunAt": "2026-03-01T00:00:00.000Z",
                    "revision": 0,
                    "scheduleComputationVersion": 3
                }}]
            }}"#
        );
        seed_project(&harness.home, project_key, &document);
        let paths = resolve_cron_paths(&harness.home.to_string_lossy(), project_key).unwrap();
        let mut lines = String::new();
        for index in 0..30 {
            lines.push_str(&format!(
                "{{\"schemaVersion\":1,\"runId\":\"{task_id}-run-{index}\",\"taskId\":\"{task_id}\",\"sessionKey\":\"cron:{task_id}\",\"startedAt\":\"2026-01-01T00:{index:02}:00.000Z\",\"outcome\":\"succeeded\"}}\n"
            ));
        }
        fs::write(&paths.run_history_file, lines).unwrap();
    };
    seed("/alpha", "task-alpha");
    seed("/beta", "task-beta");

    let body = Arc::new(RecordingBody::default());
    let gateway: Arc<dyn CronAgentGateway> = Arc::new(SessionAdmissionCronGateway::new(
        Arc::new(SessionAdmission::default()),
        body,
    ));
    harness.manager.bind_agent_gateway(gateway).unwrap();
    harness.manager.start().unwrap();

    let merged_runs = |input: JsValue| match harness
        .manager
        .list_tasks(&input)
        .unwrap()
        .get("recentRuns")
    {
        Some(JsValue::Array(runs)) => runs.len(),
        _ => panic!("recentRuns must be an array when includeHistory"),
    };
    assert_eq!(
        merged_runs(json(r#"{"includeHistory": true}"#)),
        50,
        "#1325 global list nullish limit: absent limit takes the same `?? 50` default as the per-runtime path"
    );
    assert_eq!(
        merged_runs(json(r#"{"includeHistory": true, "limit": null}"#)),
        50,
        "#1325 global list nullish limit: `??` only falls through on nullish, so an explicit null also defaults"
    );
    assert_eq!(
        merged_runs(json(r#"{"includeHistory": true, "limit": 60}"#)),
        60,
        "#1325 global list nullish limit: an explicit limit above the merged size keeps every run"
    );
    assert_eq!(
        merged_runs(json(r#"{"includeHistory": true, "limit": 0}"#)),
        0,
        "#1325 global list nullish limit: an explicit 0 still yields the empty page"
    );
}

/// A panicking body must not leave the session's admission fence behind: the
/// `TurnFence` drops on unwind, so the same session is admittable again (the
/// #1258 tail guard is the same shape one layer down in the store).
struct PanickingBody;
impl CronGatewayBody for PanickingBody {
    fn run_turn(&self, _input: &CronTurnSubmit) -> Result<(), CronError> {
        panic!("#1325 admission fence probe: the cron body panics mid-turn");
    }
    fn abort_turn(
        &self,
        _session_key: &JsString,
        _run_id: Option<&JsString>,
        _reason: Option<&JsString>,
    ) -> Result<(), CronError> {
        Ok(())
    }
    fn close_session(
        &self,
        _session_key: &JsString,
        _reason: Option<&JsString>,
    ) -> Result<(), CronError> {
        Ok(())
    }
}

#[test]
fn submit_turn_releases_the_admission_fence_when_the_body_panics() {
    let admission = Arc::new(SessionAdmission::default());
    let gateway = SessionAdmissionCronGateway::new(admission.clone(), Arc::new(PanickingBody));
    let input = CronTurnSubmit {
        session_key: JsString::from("cron:panic"),
        channel_key: JsString::from("cron"),
        project_key: None,
        message: JsString::from("m"),
        mode: JsString::from("default"),
        run_id: JsString::from("run-panic"),
        timeout_ms: 0.0,
    };
    let caught =
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| gateway.submit_turn(&input)));
    assert!(
        caught.is_err(),
        "#1325 admission fence: the body's panic must propagate out of submitTurn"
    );
    assert!(
        !admission.has_active_turn(&input.session_key),
        "#1325 admission fence: an unwinding turn must still release the session's admission"
    );
    assert!(
        admission
            .begin_turn(&input.session_key, &input.run_id)
            .is_some(),
        "#1325 admission fence: the session must be admittable again after the panic"
    );
}

// S2: the six `methods.rs` adapters are the wire call surface
// (InProcessGateway.ts:2603-2625): everything the S5 gold replay asserts
// must be reachable through them unchanged — same results, same errors.
#[test]
fn method_adapters_mirror_the_manager_surface() {
    let harness = harness(false);

    let created = methods::cron_create(&harness.manager, &create_input("/proj")).unwrap();
    let task = created.get("task").expect("create returns {task}");
    assert_eq!(text(task, "taskId"), "task-1");

    let listed = methods::cron_list(
        &harness.manager,
        &json(r#"{"projectKey": "/proj", "includeHistory": true}"#),
    )
    .unwrap();
    assert_eq!(
        match listed.get("tasks") {
            Some(JsValue::Array(tasks)) => tasks.len(),
            _ => panic!("tasks must be an array"),
        },
        1,
        "the adapter-created task is listed"
    );

    let updated = methods::cron_update(
        &harness.manager,
        &json(
            r#"{
                "taskId": "task-1",
                "projectKey": "/proj",
                "expectedRevision": 0,
                "message": "m2",
                "schedule": {"type": "cron", "expression": "5 * * * *", "timezone": "UTC"}
            }"#,
        ),
    )
    .unwrap();
    assert_eq!(
        updated.get("updated").and_then(JsValue::as_bool),
        Some(true),
        "the update goes through the adapter surface"
    );

    let started = methods::cron_run_now(
        &harness.manager,
        &json(r#"{"taskId": "task-1", "projectKey": "/proj"}"#),
    )
    .unwrap();
    assert_eq!(
        started.get("started").and_then(JsValue::as_bool),
        Some(true),
        "runTaskNow starts through the adapter surface"
    );

    let stopped = methods::cron_stop(&harness.manager, &json(r#"{"taskId": "task-1"}"#)).unwrap();
    assert_eq!(
        stopped.get("stopped").and_then(JsValue::as_bool),
        Some(false),
        "no active run, so the stop reports idle"
    );

    let deleted = methods::cron_delete(
        &harness.manager,
        &json(r#"{"taskId": "task-1", "projectKey": "/proj"}"#),
    )
    .unwrap();
    assert_eq!(
        deleted.get("deleted").and_then(JsValue::as_bool),
        Some(true),
        "the delete goes through the adapter surface"
    );
}

// S2: an absent `frame.params` crosses the WS layer as `undefined`
// (GatewayWsConnection.ts:464-470), and each adapter's first property read
// is V8's `Cannot read properties of undefined (reading '<field>')` —
// except `cron_list`, whose `input = {}` default (CronManager.ts:121)
// swallows it before the first read. The field names pin the TS read order
// at the CronManager call sites: create reads `projectKey`
// (CronManager.ts:115), update/delete/runTaskNow read `taskId` first
// (CronManager.ts:145,151,165), stop reads `projectKey` inside
// resolveStopRuntime (CronManager.ts:250).
#[test]
fn absent_params_read_as_undefined() {
    let harness = harness(false);

    type MethodFn = fn(&CronManager, &JsValue) -> Result<JsValue, CronError>;
    let cases: [(&str, MethodFn, &str); 5] = [
        ("cron_create", methods::cron_create, "projectKey"),
        ("cron_update", methods::cron_update, "taskId"),
        ("cron_delete", methods::cron_delete, "taskId"),
        ("cron_stop", methods::cron_stop, "projectKey"),
        ("cron_run_now", methods::cron_run_now, "taskId"),
    ];
    for (name, method, field) in cases {
        let error = method(&harness.manager, &JsValue::Undefined).unwrap_err();
        assert_eq!(text(&error.as_json(), "name"), "TypeError", "{name}");
        assert_eq!(
            text(&error.as_json(), "message"),
            format!("Cannot read properties of undefined (reading '{field}')"),
            "{name}"
        );
    }

    let listed = methods::cron_list(&harness.manager, &JsValue::Undefined).unwrap();
    assert_eq!(
        match listed.get("tasks") {
            Some(JsValue::Array(tasks)) => tasks.len(),
            _ => panic!("tasks must be an array"),
        },
        0,
        "the `input = {{}}` default makes cron_list alone tolerate absent params"
    );
}

// S2: a primitive `params` reads its properties as `undefined` (V8 boxes
// primitives), so `cron_create` reaches `requireProjectKey` and fails its
// `!projectKey?.trim()` branch instead of throwing (probed on Node v22.22.0).
#[test]
fn primitive_params_read_as_undefined_properties() {
    let harness = harness(false);

    let error = methods::cron_create(&harness.manager, &json(r#""params""#)).unwrap_err();
    assert_eq!(text(&error.as_json(), "name"), "Error");
    assert_eq!(
        text(&error.as_json(), "message"),
        "Cron task creation requires a projectKey."
    );
}

// S3: the `requireCron` gate over an unconfigured runtime
// (InProcessGateway.ts:3016-3022) fires before any adapter, and only for the
// six registered names (InProcessGateway.ts:2114-2122) — other methods are
// not ours to answer.
#[test]
fn projection_gates_unconfigured_runtime() {
    let harness = harness(false);
    let request =
        json(r#"{"type": "request", "id": "req-1", "method": "cron_create", "params": {}}"#);

    for method in super::projection::CRON_METHODS {
        let error = super::projection::dispatch(None, method, request.get("params").unwrap())
            .expect("a cron method answers")
            .unwrap_err();
        assert_eq!(
            text(&error.as_json(), "name"),
            "DialogGatewayError",
            "{method}"
        );
        assert_eq!(
            text(&error.as_json(), "code"),
            "CAPABILITY_UNAVAILABLE",
            "{method}"
        );
        assert_eq!(
            text(&error.as_json(), "message"),
            "Cron runtime is not configured on this gateway.",
            "{method}"
        );
    }
    assert!(
        super::projection::dispatch(None, "session_model_get", &JsValue::Undefined).is_none(),
        "a non-cron method is not ours to answer"
    );

    // Configured, the same dispatch delegates: the gate order (requireCron
    // before the adapter) shows as the adapter's own error surfacing.
    let error =
        super::projection::dispatch(Some(&harness.manager), "cron_create", &JsValue::Undefined)
            .expect("a cron method answers")
            .unwrap_err();
    assert_eq!(text(&error.as_json(), "name"), "TypeError");
}

// S3: the wire envelope (GatewayWsConnection.ts:187,236-253): success is
// `{type, id, ok, result}`; a thrown plain Error falls back to
// `gateway_request_failed` while a `DialogGatewayError` keeps its code.
#[test]
fn projection_response_envelope_shapes() {
    let request = json(r#"{"type": "request", "id": "req-1", "method": "cron_list"}"#);

    let ok = super::projection::response(&request, Ok(json(r#"{"tasks": []}"#)))
        .expect("the envelope serializes");
    assert_eq!(
        String::from_utf8(ok).unwrap(),
        r#"{"type":"response","id":"req-1","ok":true,"result":{"tasks":[]}}"#
    );

    let plain = super::projection::response(
        &request,
        Err(CronError::error(
            "Cron task creation requires a projectKey.",
        )),
    )
    .expect("the envelope serializes");
    assert_eq!(
        String::from_utf8(plain).unwrap(),
        r#"{"type":"response","id":"req-1","ok":false,"error":{"code":"gateway_request_failed","message":"Cron task creation requires a projectKey."}}"#
    );

    let gateway = super::projection::response(
        &request,
        Err(CronError::gateway(
            "CAPABILITY_UNAVAILABLE",
            "Cron runtime is not configured on this gateway.",
        )),
    )
    .expect("the envelope serializes");
    assert_eq!(
        String::from_utf8(gateway).unwrap(),
        r#"{"type":"response","id":"req-1","ok":false,"error":{"code":"CAPABILITY_UNAVAILABLE","message":"Cron runtime is not configured on this gateway."}}"#
    );
}

// ------------------------------------------------------------ S5 gold replay

/// The real-TS gold of the six-method surface (see
/// `tests/fixtures/native-cron/`), recorded at baseline cc7c4e6 / Node
/// v22.22.0 with the clock pinned at the same epoch as `PINNED_NOW_MS`, so
/// every byte below is reproducible. Provenance (commit + source hashes) is
/// verified by `crate::test_baseline::oracle`.
const NATIVE_CRON_ORACLE: &str = include_str!("../../../tests/fixtures/native-cron/oracle.json");

fn oracle_array(value: Option<&JsValue>, what: &str) -> Vec<JsValue> {
    match value {
        Some(JsValue::Array(items)) => items.clone(),
        other => panic!("#1325 oracle {what} must be an array: {other:?}"),
    }
}

fn recorded_json(value: &JsValue) -> String {
    stringify_json(value)
        .unwrap_or_else(|error| panic!("#1325 oracle bytes must serialize: {error:?}"))
        .expect("oracle values are never undefined-topped")
}

/// One gold cell: a fresh scratch home seeded from the cell's `files`, a
/// manager with the cell's config (pinned clock, counter taskIds — the
/// generator's exact injections), and the six `methods::` adapters driven
/// step by step against the recorded outcomes.
#[test]
fn six_methods_replay_the_ts_gold() {
    let oracle = crate::test_baseline::oracle(NATIVE_CRON_ORACLE);
    assert_eq!(
        text(&oracle, "node"),
        "v22.22.0",
        "#1325 the gold must come from the pinned Node"
    );
    let cells = oracle_array(oracle.get("cells"), "cells");
    assert_eq!(cells.len(), 10, "#1325 gold cell count drifted");
    for cell in &cells {
        replay_gold_cell(cell);
    }

    // The unconfigured half: every method's first frame hits the requireCron
    // gate (InProcessGateway.ts:3016-3022) before any adapter runs.
    let unconfigured = oracle_array(oracle.get("unconfigured"), "unconfigured");
    assert_eq!(unconfigured.len(), 6, "#1325 unconfigured count drifted");
    for entry in &unconfigured {
        let method = text(entry, "method");
        let wire = match method.as_str() {
            "createTask" => "cron_create",
            "listTasks" => "cron_list",
            "updateTask" => "cron_update",
            "deleteTask" => "cron_delete",
            "stopTask" => "cron_stop",
            "runTaskNow" => "cron_run_now",
            other => panic!("#1325 gold method {other} is not one of the six"),
        };
        let error = dispatch(None, wire, &JsValue::Undefined)
            .unwrap_or_else(|| panic!("#1325 {method} must be a cron method"))
            .expect_err("#1325 the unconfigured gate must reject");
        let recorded = entry
            .get("error")
            .unwrap_or_else(|| panic!("#1325 unconfigured {method} must record an error"));
        assert_eq!(
            recorded_json(&error.as_json()),
            recorded_json(recorded),
            "#1325 unconfigured {method} error differs from the TS gold"
        );
    }
}

fn replay_gold_cell(cell: &JsValue) {
    let name = text(cell, "name");
    let scratch = Scratch::new("cron-manager-gold");
    let home = scratch.path().join("home");
    fs::create_dir_all(&home).unwrap();
    for file in oracle_array(cell.get("files"), "cell files") {
        let path = home.join(text(&file, "path"));
        fs::create_dir_all(path.parent().expect("seeded files sit in a directory"))
            .unwrap_or_else(|error| panic!("#1325 gold {name}: seed {}: {error}", path.display()));
        fs::write(&path, text(&file, "text"))
            .unwrap_or_else(|error| panic!("#1325 gold {name}: seed {}: {error}", path.display()));
    }
    let config = match cell.get("config") {
        Some(config @ JsValue::Object(_)) => CronManagerConfig {
            enabled: config
                .get("enabled")
                .and_then(JsValue::as_bool)
                .unwrap_or(true),
            timezone: text(config, "timezone"),
            max_concurrent_runs: config
                .get("maxConcurrentRuns")
                .and_then(JsValue::as_number)
                .unwrap_or(1.0),
            run_timeout_minutes: config
                .get("runTimeoutMinutes")
                .and_then(JsValue::as_number)
                .unwrap_or(60.0),
        },
        _ => CronManagerConfig::default(),
    };
    let next_task = Arc::new(AtomicUsize::new(0));
    let manager = CronManager::new(CronManagerOptions {
        config,
        pilot_home: home.to_string_lossy().into_owned(),
        session_overrides: None,
        now: Some(Arc::new(|| PINNED_NOW_MS)),
        uuid: Some(Arc::new(move || {
            JsString::from(format!(
                "task-{}",
                next_task.fetch_add(1, Ordering::SeqCst) + 1
            ))
        })),
        logger: Some(Arc::new(RecordingLogger::default())),
        scheduler_idle_poll_ms: None,
    })
    .unwrap();

    let steps = oracle_array(cell.get("steps"), "cell steps");
    let results = oracle_array(cell.get("results"), "cell results");
    assert_eq!(
        steps.len(),
        results.len(),
        "#1325 gold {name}: every step must have a recorded result"
    );
    for (index, (step, recorded)) in steps.iter().zip(&results).enumerate() {
        let method = text(step, "method");
        let input = step.get("input").cloned().unwrap_or(JsValue::Undefined);
        let outcome = match method.as_str() {
            "createTask" => methods::cron_create(&manager, &input),
            "listTasks" => methods::cron_list(&manager, &input),
            "updateTask" => methods::cron_update(&manager, &input),
            "deleteTask" => methods::cron_delete(&manager, &input),
            "stopTask" => methods::cron_stop(&manager, &input),
            "runTaskNow" => methods::cron_run_now(&manager, &input),
            other => panic!("#1325 gold {name} step {index}: method {other} is not one of the six"),
        };
        let ok = recorded
            .get("ok")
            .and_then(JsValue::as_bool)
            .unwrap_or_else(|| panic!("#1325 gold {name} step {index}: ok must be boolean"));
        match (outcome, ok) {
            (Ok(result), true) => assert_eq!(
                recorded_json(&result),
                text(recorded, "json"),
                "#1325 gold {name} step {index} ({method}) differs from the TS gold"
            ),
            (Err(error), false) => {
                let recorded_error = recorded.get("error").unwrap_or_else(|| {
                    panic!("#1325 gold {name} step {index}: error must be recorded")
                });
                assert_eq!(
                    recorded_json(&error.as_json()),
                    recorded_json(recorded_error),
                    "#1325 gold {name} step {index} ({method}) error differs from the TS gold"
                );
            }
            (outcome, ok) => panic!(
                "#1325 gold {name} step {index} ({method}): expected {}, got {outcome:?}",
                if ok { "a value" } else { "an error" },
            ),
        }
    }
}

// The run-now success step stays byte-exact because its result carries no
// timestamp: the once task it creates is stamped by runTaskNow's `new Date()`
// (CronRuntime.ts:396) — the injected `self.now`, hence the pinned test clock
// — and no gold step reads that task back. The port reads that clock once per
// run-now (#2047) rather than twice, so a millisecond tick can no longer flip
// a valid run-now into the "One-time Cron tasks must be scheduled in the
// future." rejection.

/// #280: the committed oracle must be what the TS generator produces.
#[test]
#[ignore = "requires fixed PDRS_BASELINE and Node v22.22.0"]
fn native_cron_oracle_regenerates_identically() {
    crate::test_baseline::regenerates_identically("native-cron");
}
