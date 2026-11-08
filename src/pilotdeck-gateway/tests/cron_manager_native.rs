//! #1325 cron manager native seam fixtures: repeat-trigger, cancel, and
//! process-restart, replayed over the real cron store and a real
//! `SessionAdmission` from the committed trees in
//! `tests/fixtures/native-cron/`.
//!
//! The three seams this issue owns from the #2295 CronFire/CronScheduler
//! contract:
//! - the active-run registry (`registerActiveRun`/`unregisterActiveRun`,
//!   CronRuntime.ts:412-420) — the cancel and stop-drain paths observe it;
//! - `runTaskNow`'s `already_running` guard (CronRuntime.ts:392) — a repeat
//!   trigger while a run is live never re-executes;
//! - `recoverInterruptedRuns` (CronRuntime.ts:502-562) — a restart records
//!   the interrupted run as failed instead of re-executing it.
//!
//! `submit_turn` is synchronous in this port (consuming the admitted turn's
//! event stream is the fire half's, fire.rs #2295), so the duplicate-submit fixture re-enters
//! the same admission from inside the body's `run_turn` — the exact shape
//! the TS `SessionRouter.beginTurn` rejection sees when the scheduler
//! fires twice before the first turn unwinds.
use std::{
    fs,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, OnceLock, Weak},
};

use pilotdeck_gateway::runtime::{
    cron::{
        CronActiveRun, CronAgentGateway, CronError, CronGatewayBody, CronLogger, CronManager,
        CronManagerConfig, CronManagerOptions, CronTurnSubmit, SessionAdmissionCronGateway,
        resolve_cron_paths,
    },
    session::SessionAdmission,
};
use pilotdeck_protocol::{JsObject, JsString, JsValue, parse_json, stringify_json};

#[path = "common/scratch.rs"]
mod scratch;

use scratch::Scratch;

/// `installFixedClock()`'s default epoch (`Date.UTC(2026, 0, 1)`), the pin
/// the #1216 store oracle already uses.
const PINNED_NOW_MS: f64 = 1_767_225_600_000.0;

const REPEAT_TRIGGER: &str = include_str!("fixtures/native-cron/repeat-trigger.json");
const CANCEL_ONCE: &str = include_str!("fixtures/native-cron/cancel-once.json");
const RESTART_UNRECORDED: &str = include_str!("fixtures/native-cron/restart-unrecorded.json");
const RESTART_TERMINAL: &str = include_str!("fixtures/native-cron/restart-terminal.json");
const RESTART_TERMINAL_HISTORY: &str =
    include_str!("fixtures/native-cron/restart-terminal-history.jsonl");

fn json(raw: &str) -> JsValue {
    parse_json(raw).unwrap_or_else(|error| panic!("fixture JSON must parse: {error:?}"))
}

fn text(value: &JsValue, field: &str) -> String {
    value
        .get(field)
        .and_then(JsValue::as_string)
        .unwrap_or_else(|| panic!("fixture field {field} must be a string"))
        .to_utf8()
        .expect("fixture strings are UTF-8")
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
    #[allow(dead_code)]
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

/// The repeat-trigger admission fixture: the second trigger for the same
/// session arrives from inside the first turn's `run_turn`, the only point
/// where the synchronous port can observe the TS event-stream overlap.
struct ReentrantBody {
    calls: Mutex<Vec<String>>,
    gateway: OnceLock<Weak<SessionAdmissionCronGateway>>,
}
impl ReentrantBody {
    fn new() -> Self {
        Self {
            calls: Mutex::new(Vec::new()),
            gateway: OnceLock::new(),
        }
    }
    fn record(&self, line: String) {
        self.calls.lock().unwrap().push(line);
    }
    fn calls(&self) -> Vec<String> {
        self.calls.lock().unwrap().clone()
    }
}
impl CronGatewayBody for ReentrantBody {
    fn run_turn(&self, input: &CronTurnSubmit) -> Result<(), CronError> {
        self.record(format!(
            "run_turn {} {}",
            input.session_key.to_utf8().unwrap(),
            input.run_id.to_utf8().unwrap(),
        ));
        // The scheduler's second trigger: same session, fresh run id.
        let gateway = self
            .gateway
            .get()
            .and_then(Weak::upgrade)
            .expect("gateway registered before the first submit");
        let second = CronTurnSubmit {
            session_key: input.session_key.clone(),
            channel_key: input.channel_key.clone(),
            project_key: input.project_key.clone(),
            message: input.message.clone(),
            mode: input.mode.clone(),
            run_id: JsString::from("run-second"),
            timeout_ms: input.timeout_ms,
        };
        let rejected = gateway.submit_turn(&second);
        let verdict = match &rejected {
            Ok(()) => "accepted".to_owned(),
            Err(error) => format!(
                "{} {}",
                error.code().unwrap_or("-"),
                error.message().to_utf8().unwrap(),
            ),
        };
        self.record(format!("second_submit {verdict}"));
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

/// One manager over its own scratch home, with pinned clock and counter
/// taskIds. Built unstarted and unbound: each fixture binds the gateway its
/// scenario needs before any start.
struct Harness {
    home: PathBuf,
    manager: Arc<CronManager>,
    logger: Arc<RecordingLogger>,
    /// Dropped last, so the scratch tree outlives the manager's file writes.
    _scratch: Scratch,
}

fn harness() -> Harness {
    let scratch = Scratch::new("cron-manager-native");
    let home = scratch.path().join("home");
    fs::create_dir_all(&home).unwrap();
    let logger = Arc::new(RecordingLogger::default());
    let next_task = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let manager = CronManager::new(CronManagerOptions {
        config: CronManagerConfig::default(),
        pilot_home: home.to_string_lossy().into_owned(),
        session_overrides: None,
        now: Some(Arc::new(|| PINNED_NOW_MS)),
        uuid: Some(Arc::new(move || {
            JsString::from(format!(
                "task-{}",
                next_task.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1
            ))
        })),
        logger: Some(logger.clone()),
        scheduler_idle_poll_ms: None,
    })
    .unwrap();
    Harness {
        home,
        manager,
        logger,
        _scratch: scratch,
    }
}

/// Seed a project's `tasks.json` fixture plus the `.cwd` marker, exactly
/// where `resolve_cron_paths` puts them.
fn seed_fixture(home: &Path, project_key: &str, tasks: &str) {
    let paths = resolve_cron_paths(&home.to_string_lossy(), project_key).unwrap();
    fs::create_dir_all(&paths.project_dir).unwrap();
    fs::write(&paths.tasks_file, tasks).unwrap();
    fs::write(paths.project_dir.join(".cwd"), format!("{project_key}\n")).unwrap();
}

/// Seed a project's committed run-history lines.
fn seed_history(home: &Path, project_key: &str, history: &str) {
    let paths = resolve_cron_paths(&home.to_string_lossy(), project_key).unwrap();
    fs::write(&paths.run_history_file, history).unwrap();
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

fn submit(session: &str, run: &str) -> CronTurnSubmit {
    CronTurnSubmit {
        session_key: JsString::from(session),
        channel_key: JsString::from("cron"),
        project_key: Some(JsString::from("/repeat")),
        message: JsString::from("repeat me"),
        mode: JsString::from("default"),
        run_id: JsString::from(run),
        timeout_ms: 0.0,
    }
}

// Matrix cell 6: a repeat trigger (`runTaskNow`) while the task's run is
// still live returns `already_running` — the second run never starts.
#[test]
fn repeat_trigger_does_not_re_execute() {
    let harness = harness();
    seed_fixture(&harness.home, "/repeat", REPEAT_TRIGGER);

    let body = Arc::new(RecordingBody::default());
    let gateway: Arc<dyn CronAgentGateway> = Arc::new(SessionAdmissionCronGateway::new(
        Arc::new(SessionAdmission::default()),
        body.clone(),
    ));
    harness.manager.bind_agent_gateway(gateway).unwrap();

    let started = harness
        .manager
        .run_task_now(&json(
            r#"{"taskId": "task-repeat", "projectKey": "/repeat"}"#,
        ))
        .unwrap();
    assert_eq!(
        started.get("started").and_then(JsValue::as_bool),
        Some(false),
        "the running task refuses a second concurrent run"
    );
    assert_eq!(text(&started, "reason"), "already_running");
    assert_eq!(text(&started, "taskId"), "task-repeat");
    assert!(
        body.calls().is_empty(),
        "no turn was submitted: {:?}",
        body.calls()
    );
}

// Matrix cell 11: the second trigger for a session that already holds an
// admitted turn is rejected by the same admission fence — the rejected
// submit surfaces as the terminal `session_busy` gateway error
// (InProcessGateway.ts:1043-1067).
#[test]
fn repeat_trigger_second_submit_rejected_by_admission() {
    let harness = harness();
    seed_fixture(&harness.home, "/repeat", REPEAT_TRIGGER);

    let body = Arc::new(ReentrantBody::new());
    let admission = Arc::new(SessionAdmission::default());
    let gateway = Arc::new(SessionAdmissionCronGateway::new(admission, body.clone()));
    body.gateway
        .set(Arc::downgrade(&gateway))
        .expect("gateway registered once");
    harness
        .manager
        .bind_agent_gateway(gateway.clone() as Arc<dyn CronAgentGateway>)
        .unwrap();

    gateway
        .submit_turn(&submit("cron:task-repeat", "run-repeat"))
        .expect("the first trigger is admitted");

    let calls = body.calls();
    assert_eq!(
        calls,
        [
            "run_turn cron:task-repeat run-repeat".to_owned(),
            "second_submit session_busy Session cron:task-repeat already has an active turn."
                .to_owned(),
        ],
        "the second trigger is rejected inside the first turn's admission fence"
    );
}

// Matrix cell 8: cancelling an in-flight run (`stopTask`) marks the run
// stop-requested, aborts the turn with no reason, deletes the once task,
// and releases its session.
#[test]
fn cancel_aborts_run_and_deletes_once_task() {
    let harness = harness();
    seed_fixture(&harness.home, "/cancel", CANCEL_ONCE);

    let body = Arc::new(RecordingBody::default());
    let gateway: Arc<dyn CronAgentGateway> = Arc::new(SessionAdmissionCronGateway::new(
        Arc::new(SessionAdmission::default()),
        body.clone(),
    ));
    harness.manager.bind_agent_gateway(gateway).unwrap();

    // Load the runtime (listTasks ensures it) so the projectKey-less stop
    // selector finds it by `lastRunId` — the shape the WS client sends.
    harness
        .manager
        .list_tasks(&json(r#"{"projectKey": "/cancel"}"#))
        .unwrap();
    let runtime = harness.manager.loaded_runtime("/cancel").unwrap();
    runtime.register_active_run(CronActiveRun {
        run_id: JsString::from("run-cancel"),
        task_id: JsString::from("task-cancel"),
        session_key: JsString::from("cron:task-cancel"),
        schedule_type: JsString::from("once"),
        stop_requested: false,
    });

    let stopped = harness
        .manager
        .stop_task(&json(r#"{"taskId": "task-cancel", "runId": "run-cancel"}"#))
        .unwrap();
    assert_eq!(
        stopped.get("stopped").and_then(JsValue::as_bool),
        Some(true),
        "the active run is stopped"
    );
    assert_eq!(text(&stopped, "taskId"), "task-cancel");
    assert_eq!(text(&stopped, "runId"), "run-cancel");
    assert_eq!(
        stopped.get("deletedOneTimeTask").and_then(JsValue::as_bool),
        Some(true),
        "the once task is deleted with its cancelled run"
    );
    assert!(
        read_tasks(&harness.home, "/cancel").is_empty(),
        "the once task is gone from the store"
    );
    assert_eq!(
        body.calls(),
        [
            "abort_turn cron:task-cancel run-cancel -".to_owned(),
            "close_session cron:task-cancel cron/task-removed".to_owned(),
        ],
        "abortTurn carries no reason; the session releases as task-removed"
    );
    assert!(
        runtime
            .active_run(&JsString::from("run-cancel"))
            .is_some_and(|run| run.stop_requested),
        "the registry entry stays for the run's own finally (CronFire, fire.rs)"
    );
}

// Matrix cell 13: a process restart (a fresh manager start) over a crashed
// mid-run task records the interrupted run as failed and reschedules the
// task — nothing re-executes.
#[test]
fn restart_records_interrupted_run_without_re_execution() {
    let harness = harness();
    seed_fixture(&harness.home, "/restart", RESTART_UNRECORDED);

    let body = Arc::new(RecordingBody::default());
    let gateway: Arc<dyn CronAgentGateway> = Arc::new(SessionAdmissionCronGateway::new(
        Arc::new(SessionAdmission::default()),
        body.clone(),
    ));
    harness.manager.bind_agent_gateway(gateway).unwrap();
    harness.manager.start().unwrap();

    let history = run_history(&harness.home, "/restart");
    assert_eq!(history.len(), 1, "exactly the recovery record");
    assert_eq!(text(&history[0], "runId"), "run-crash");
    assert_eq!(text(&history[0], "outcome"), "failed");
    assert_eq!(
        text(history[0].get("error").unwrap(), "code"),
        "cron_run_interrupted"
    );
    assert_eq!(
        text(&history[0], "startedAt"),
        "2026-01-01T00:00:00.000Z",
        "the record starts at the stored task.updatedAt"
    );

    let tasks = read_tasks(&harness.home, "/restart");
    let task = tasks
        .iter()
        .find(|task| text(task, "taskId") == "task-restart")
        .expect("recovered task on disk");
    assert_eq!(text(task, "status"), "scheduled");
    assert_eq!(
        task.get("revision").and_then(JsValue::as_number),
        Some(2.0),
        "the reschedule bumps the revision"
    );
    assert_eq!(
        text(task, "nextRunAt"),
        "2026-01-01T00:05:00.000Z",
        "the next run is recomputed from the pinned now"
    );
    assert!(
        body.calls()
            .iter()
            .all(|call| !call.starts_with("run_turn")),
        "the interrupted run is never re-executed: {:?}",
        body.calls()
    );
    assert!(
        harness
            .logger
            .messages()
            .iter()
            .any(|line| line.starts_with("warn cron runtime recovered interrupted runs")),
        "the recovery is logged with its recoveredCount"
    );
}

// Matrix cell 14: when the crashed task's `lastRunId` already has a
// terminal record, the restart only reschedules — no second record.
#[test]
fn restart_with_terminal_run_reschedules_only() {
    let harness = harness();
    seed_fixture(&harness.home, "/restart-terminal", RESTART_TERMINAL);
    seed_history(&harness.home, "/restart-terminal", RESTART_TERMINAL_HISTORY);

    let body = Arc::new(RecordingBody::default());
    let gateway: Arc<dyn CronAgentGateway> = Arc::new(SessionAdmissionCronGateway::new(
        Arc::new(SessionAdmission::default()),
        body.clone(),
    ));
    harness.manager.bind_agent_gateway(gateway).unwrap();
    harness.manager.start().unwrap();

    let history = run_history(&harness.home, "/restart-terminal");
    assert_eq!(
        history.len(),
        1,
        "the terminal record stands alone — no duplicate recovery record"
    );
    assert_eq!(text(&history[0], "runId"), "run-done");

    let tasks = read_tasks(&harness.home, "/restart-terminal");
    let task = tasks
        .iter()
        .find(|task| text(task, "taskId") == "task-restart-terminal")
        .expect("recovered task on disk");
    assert_eq!(text(task, "status"), "scheduled");
    assert_eq!(
        task.get("revision").and_then(JsValue::as_number),
        Some(2.0),
        "the reschedule still bumps the revision"
    );
    assert!(
        body.calls()
            .iter()
            .all(|call| !call.starts_with("run_turn")),
        "nothing re-executes: {:?}",
        body.calls()
    );
}
