//! #2295 scheduler/fire matrix tests: the golden replay of the real-TS
//! fixture (`tests/fixtures/cron-scheduler/`, ten started-manager cells over
//! mock timers — see that directory's README for the S/T matrix) plus the
//! lifecycle and classification cells that need injected seams instead of an
//! oracle (gate constants, poke-while-in-flight, the fire classification
//! ladder over the synchronous submit boundary).
use std::{
    fs,
    path::PathBuf,
    sync::{
        Arc, Condvar, Mutex, Weak,
        atomic::{AtomicBool, AtomicUsize, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};

use pilotdeck_protocol::{
    JsObject, JsString, JsValue, parse_json, quote_json_string, stringify_json,
};

use super::{
    CronActiveRun, CronAgentGateway, CronError, CronFire, CronFireDeps, CronFirePort, CronLogger,
    CronManager, CronManagerConfig, CronManagerOptions, CronScheduler, CronTaskStore,
    CronTurnSubmit, CronZoneResolverCache, DEFAULT_IDLE_POLL_MS, SchedulerDeps, resolve_cron_paths,
};
use crate::test_scratch::Scratch;

/// `installFixedClock()`'s default epoch (`Date.UTC(2026, 0, 1)`), the pin
/// the generator ran under — every replayed timestamp is byte-stable.
const PINNED_NOW_MS: f64 = 1_767_225_600_000.0;
const ORACLE: &str = include_str!("../../../tests/fixtures/cron-scheduler/oracle.json");

fn json(raw: &str) -> JsValue {
    parse_json(raw).unwrap_or_else(|error| panic!("#2295 fixture JSON must parse: {error:?}"))
}

fn text(value: &JsValue, field: &str) -> String {
    value
        .get(field)
        .and_then(JsValue::as_string)
        .unwrap_or_else(|| panic!("#2295 fixture field {field} must be a string"))
        .to_utf8()
        .expect("#2295 fixture strings are UTF-8")
}

fn oracle_array(value: Option<&JsValue>, what: &str) -> Vec<JsValue> {
    match value {
        Some(JsValue::Array(items)) => items.clone(),
        other => panic!("#2295 oracle {what} must be an array: {other:?}"),
    }
}

fn string_array(value: Option<&JsValue>, what: &str) -> Vec<String> {
    oracle_array(value, what)
        .iter()
        .map(|value| {
            value
                .as_string()
                .unwrap_or_else(|| panic!("#2295 oracle {what} entries are strings"))
                .to_utf8()
                .expect("#2295 oracle strings are UTF-8")
        })
        .collect()
}

// ------------------------------------------------------------ shared harness

/// The logger slice the observations compare: `level message` text only —
/// the EISDIR error metadata of the tick-failure cell is host-specific, so
/// the oracle records distinct message sets, never counts or metadata.
#[derive(Default)]
struct MessageLogger {
    messages: Mutex<Vec<String>>,
}
impl CronLogger for MessageLogger {
    fn info(&self, message: &str, _meta: &JsObject) {
        self.messages
            .lock()
            .unwrap()
            .push(format!("info {message}"));
    }
    fn warn(&self, message: &str, _meta: &JsObject) {
        self.messages
            .lock()
            .unwrap()
            .push(format!("warn {message}"));
    }
}
impl MessageLogger {
    fn distinct(&self, level: &str) -> Vec<String> {
        let mut seen: Vec<String> = self
            .messages
            .lock()
            .unwrap()
            .iter()
            .filter(|line| line.starts_with(level))
            .cloned()
            .collect();
        seen.sort();
        seen.dedup();
        seen
    }
    fn infos(&self) -> Vec<String> {
        self.distinct("info ")
    }
    fn warns(&self) -> Vec<String> {
        self.distinct("warn ")
    }
}

/// JSON.stringify of the three port inputs, in the TS call sites' own literal
/// key order (CronFire.ts:96-104, CronRuntime.ts:373,489) — the exact bytes
/// the oracle's `gatewayCalls` lines carry.
fn submit_json(input: &CronTurnSubmit) -> String {
    let mut out = format!(
        "{{\"sessionKey\":{},\"channelKey\":{}",
        quote_json_string(&input.session_key),
        quote_json_string(&input.channel_key),
    );
    if let Some(project_key) = &input.project_key {
        out.push_str(&format!(
            ",\"projectKey\":{}",
            quote_json_string(project_key)
        ));
    }
    out.push_str(&format!(
        ",\"message\":{},\"mode\":{},\"runId\":{},\"timeoutMs\":{}}}",
        quote_json_string(&input.message),
        quote_json_string(&input.mode),
        quote_json_string(&input.run_id),
        crate::js_value::number_string(input.timeout_ms),
    ));
    out
}

fn abort_json(
    session_key: &JsString,
    run_id: Option<&JsString>,
    reason: Option<&JsString>,
) -> String {
    let mut out = format!("{{\"sessionKey\":{}", quote_json_string(session_key));
    if let Some(run_id) = run_id {
        out.push_str(&format!(",\"runId\":{}", quote_json_string(run_id)));
    }
    if let Some(reason) = reason {
        out.push_str(&format!(",\"reason\":{}", quote_json_string(reason)));
    }
    out.push('}');
    out
}

fn close_json(session_key: &JsString, reason: Option<&JsString>) -> String {
    let mut out = format!("{{\"sessionKey\":{}", quote_json_string(session_key));
    if let Some(reason) = reason {
        out.push_str(&format!(",\"reason\":{}", quote_json_string(reason)));
    }
    out.push('}');
    out
}

/// The injected `submit` seam of the fake gateway below.
type SubmitHook = Box<dyn Fn(&CronTurnSubmit) -> Result<(), CronError> + Send + Sync>;

/// The event-free turn gateway: every facade call is recorded as the port's
/// JSON input, and the injected `submit` closure decides the turn's result
/// (the default is a completed event-free turn — the shape whose artifacts
/// the Rust port's synchronous boundary produces).
struct Gateway {
    calls: Mutex<Vec<String>>,
    submit: Mutex<SubmitHook>,
}
impl Gateway {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            calls: Mutex::new(Vec::new()),
            submit: Mutex::new(Box::new(|_| Ok(()))),
        })
    }
    fn record(&self, line: String) {
        self.calls.lock().unwrap().push(line);
    }
    fn calls(&self) -> Vec<String> {
        self.calls.lock().unwrap().clone()
    }
    /// Replace the submit behavior (the parked/erroring bodies of the
    /// classification cells).
    fn set_submit(
        &self,
        body: impl Fn(&CronTurnSubmit) -> Result<(), CronError> + Send + Sync + 'static,
    ) {
        *self.submit.lock().unwrap() = Box::new(body);
    }
}
impl CronAgentGateway for Gateway {
    fn submit_turn(&self, input: &CronTurnSubmit) -> Result<(), CronError> {
        self.record(format!("submitTurn {}", submit_json(input)));
        (self.submit.lock().unwrap())(input)
    }
    fn abort_turn(
        &self,
        session_key: &JsString,
        run_id: Option<&JsString>,
        reason: Option<&JsString>,
    ) -> Result<(), CronError> {
        self.record(format!(
            "abortTurn {}",
            abort_json(session_key, run_id, reason)
        ));
        Ok(())
    }
    fn close_session(
        &self,
        session_key: &JsString,
        reason: Option<&JsString>,
    ) -> Result<(), CronError> {
        self.record(format!("closeSession {}", close_json(session_key, reason)));
        Ok(())
    }
}

/// The comparable snapshot of one observation point.
#[derive(Clone, Debug, PartialEq)]
struct Observed {
    calls: Vec<String>,
    tasks_json: Option<String>,
    history: Vec<String>,
    infos: Vec<String>,
    warns: Vec<String>,
}

struct Harness {
    home: PathBuf,
    manager: Arc<CronManager>,
    gateway: Arc<Gateway>,
    logger: Arc<MessageLogger>,
    tasks_file: PathBuf,
    history_file: PathBuf,
    /// Dropped last, so the scratch tree outlives the manager's file writes.
    _scratch: Scratch,
}

impl Harness {
    fn observe(&self) -> Observed {
        Observed {
            calls: self.gateway.calls(),
            tasks_json: fs::read_to_string(&self.tasks_file).ok(),
            history: fs::read_to_string(&self.history_file)
                .map(|raw| {
                    raw.lines()
                        .filter(|line| !line.trim().is_empty())
                        .map(str::to_owned)
                        .collect()
                })
                .unwrap_or_default(),
            infos: self.logger.infos(),
            warns: self.logger.warns(),
        }
    }
}

fn harness(config: CronManagerConfig, idle_poll_ms: Option<u64>) -> Harness {
    harness_with_now(config, idle_poll_ms, Arc::new(|| PINNED_NOW_MS))
}

/// [`harness`] with a custom clock seam: the pre-register window test parks
/// the fire thread inside `now` so a stop can land between the tick's
/// pre-register and the fire's re-register.
fn harness_with_now(
    config: CronManagerConfig,
    idle_poll_ms: Option<u64>,
    now: Arc<dyn Fn() -> f64 + Send + Sync>,
) -> Harness {
    let scratch = Scratch::new("cron-scheduler");
    let home = scratch.path().join("home");
    fs::create_dir_all(&home).unwrap();
    let paths = resolve_cron_paths(&home.to_string_lossy(), "/fire").unwrap();
    let next_id = Arc::new(AtomicUsize::new(0));
    let logger = Arc::new(MessageLogger::default());
    let manager = CronManager::new(CronManagerOptions {
        config,
        pilot_home: home.to_string_lossy().into_owned(),
        session_overrides: None,
        now: Some(now),
        uuid: Some(Arc::new(move || {
            JsString::from(format!("id-{}", next_id.fetch_add(1, Ordering::SeqCst) + 1))
        })),
        logger: Some(logger.clone()),
        scheduler_idle_poll_ms: idle_poll_ms,
    })
    .unwrap();
    let gateway = Gateway::new();
    manager.bind_agent_gateway(gateway.clone()).unwrap();
    Harness {
        home,
        manager,
        gateway,
        logger,
        tasks_file: paths.tasks_file.clone(),
        history_file: paths.run_history_file.clone(),
        _scratch: scratch,
    }
}

/// Seed the `/fire` project's tasks.json (the generator's exact field set —
/// the store normalizes on read, so the byte order only needs to parse).
fn seed_tasks(harness: &Harness, bodies: &[String]) {
    let paths = resolve_cron_paths(&harness.home.to_string_lossy(), "/fire").unwrap();
    fs::create_dir_all(&paths.project_dir).unwrap();
    fs::write(
        &paths.tasks_file,
        format!("{{\"schemaVersion\":1,\"tasks\":[{}]}}\n", bodies.join(",")),
    )
    .unwrap();
}

fn once_body(task_id: &str, next_run_at: &str) -> String {
    format!(
        concat!(
            "{{\"schemaVersion\":1,\"taskId\":\"{id}\",\"message\":\"seeded\",",
            "\"schedule\":{{\"type\":\"once\",\"runAt\":\"{at}\"}},\"status\":\"scheduled\",",
            "\"sessionKey\":\"cron:{id}\",\"channelKey\":\"cron\",\"projectKey\":\"/fire\",",
            "\"mode\":\"default\",\"timezone\":\"UTC\",",
            "\"createdAt\":\"2025-12-31T00:00:00.000Z\",\"updatedAt\":\"2025-12-31T00:00:00.000Z\",",
            "\"nextRunAt\":\"{at}\",\"revision\":0}}"
        ),
        id = task_id,
        at = next_run_at
    )
}

fn cron_body(task_id: &str, next_run_at: &str) -> String {
    format!(
        concat!(
            "{{\"schemaVersion\":1,\"taskId\":\"{id}\",\"message\":\"seeded\",",
            "\"schedule\":{{\"type\":\"cron\",\"expression\":\"*/5 * * * *\",\"timezone\":\"UTC\"}},",
            "\"status\":\"scheduled\",\"sessionKey\":\"cron:{id}\",\"channelKey\":\"cron\",",
            "\"projectKey\":\"/fire\",\"mode\":\"default\",\"timezone\":\"UTC\",",
            "\"createdAt\":\"2025-12-31T00:00:00.000Z\",\"updatedAt\":\"2025-12-31T00:00:00.000Z\",",
            "\"nextRunAt\":\"{at}\",\"revision\":0,\"scheduleComputationVersion\":3}}"
        ),
        id = task_id,
        at = next_run_at
    )
}

const DUE: &str = "2025-12-31T00:00:00.000Z";
const FAR: &str = "2027-01-01T00:00:00.000Z";

/// Poll the observation until it holds still for four consecutive reads —
/// the Rust stand-in for the generator's settle pump (the fires are threads
/// here, not microtask chains), bounded so a stuck fire fails the test
/// instead of hanging it.
fn wait_settled(harness: &Harness) -> Observed {
    let mut stable = 0;
    let mut last: Option<Observed> = None;
    let start = Instant::now();
    while stable < 4 {
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "#2295 the fire chain never settled: {last:?}"
        );
        let current = harness.observe();
        if last.as_ref() == Some(&current) {
            stable += 1;
        } else {
            stable = 0;
        }
        last = Some(current);
        thread::sleep(Duration::from_millis(25));
    }
    last.expect("at least one observation")
}

fn wait_until<T>(what: &str, probe: impl Fn() -> Option<T>) -> T {
    let start = Instant::now();
    loop {
        if let Some(found) = probe() {
            return found;
        }
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "#2295 timed out waiting for {what}"
        );
        thread::sleep(Duration::from_millis(10));
    }
}

// ---------------------------------------------------------- gold replay

/// The per-cell replay timing: the TS cell's `advance` fast-forwards the
/// mocked timer clock by its exact milliseconds; the Rust stand-in injects a
/// small idle poll (the seam `CronManagerOptions.scheduler_idle_poll_ms`)
/// and sleeps the listed durations so the same ticks land — each sleep at
/// least one poll so exactly the intended ticks run, and the total short of
/// the next one so no stray tick races a fire's store claim. The 60s/250ms
/// constants themselves are pinned by the unit tests below, not by sleeps.
fn cell_timing(name: &str) -> (u64, Vec<u64>) {
    match name {
        "due-once-fires" | "due-cron-reschedules" | "not-due-no-fire" | "stop-no-fire" => {
            (500, vec![750])
        }
        "poke-fires-early" => (10_000, vec![600]),
        "no-poke-no-early-fire" => (1_000, vec![250, 1_200]),
        "tick-failure-recovers" | "rearms-after-tick" | "capacity-delays-second-task" => {
            (500, vec![750, 750])
        }
        "tick-once-manager-level" => (10_000, Vec::new()),
        other => panic!("#2295 unknown gold cell {other}"),
    }
}

#[test]
fn scheduler_replays_the_ts_gold() {
    let oracle = crate::test_baseline::oracle(ORACLE);
    assert_eq!(
        text(&oracle, "node"),
        "v22.22.0",
        "#2295 the gold must come from the pinned Node"
    );
    let cells = oracle_array(oracle.get("cells"), "cells");
    assert_eq!(cells.len(), 10, "#2295 gold cell count drifted");
    for cell in &cells {
        replay_gold_cell(cell);
    }
}

fn replay_gold_cell(cell: &JsValue) {
    let name = text(cell, "name");
    let (idle_poll_ms, advance_ms) = cell_timing(&name);
    let harness = harness(CronManagerConfig::default(), Some(idle_poll_ms));
    for file in oracle_array(cell.get("files"), "cell files") {
        let path = harness.home.join(text(&file, "path"));
        fs::create_dir_all(
            path.parent()
                .expect("#2295 seeded files sit in a directory"),
        )
        .unwrap_or_else(|error| panic!("#2295 gold {name}: seed {}: {error}", path.display()));
        fs::write(&path, text(&file, "text"))
            .unwrap_or_else(|error| panic!("#2295 gold {name}: seed {}: {error}", path.display()));
    }
    let actions = oracle_array(cell.get("actions"), "cell actions");
    let results = oracle_array(cell.get("results"), "cell results");
    assert_eq!(
        actions.len(),
        results.len(),
        "#2295 gold {name}: every action must have a recorded result"
    );
    let mut advance_index = 0usize;
    let mut observations: Vec<(String, Observed)> = Vec::new();
    for (index, (action, recorded)) in actions.iter().zip(&results).enumerate() {
        let kind = text(action, "t");
        let outcome: Result<Option<JsValue>, CronError> = match kind.as_str() {
            "start" => harness.manager.start().map(|_| None),
            "stop" => harness.manager.stop().map(|_| None),
            "tickOnce" => harness
                .manager
                .run_tick_once(
                    action
                        .get("projectKey")
                        .and_then(JsValue::as_string)
                        .map(|key| key.to_utf8().expect("#2295 project keys are UTF-8"))
                        .as_deref(),
                )
                .map(|_| None),
            "create" => harness
                .manager
                .create_task(action.get("input").unwrap_or(&JsValue::Undefined))
                .map(Some),
            "advance" => {
                let sleep = advance_ms.get(advance_index).unwrap_or_else(|| {
                    panic!("#2295 gold {name}: advance {advance_index} has no replay sleep")
                });
                advance_index += 1;
                thread::sleep(Duration::from_millis(*sleep));
                Ok(None)
            }
            "breakTasks" => {
                let backup = harness.tasks_file.with_extension("json.bak");
                fs::rename(&harness.tasks_file, &backup)
                    .unwrap_or_else(|error| panic!("#2295 gold {name}: break: {error}"));
                fs::create_dir(&harness.tasks_file)
                    .unwrap_or_else(|error| panic!("#2295 gold {name}: break: {error}"));
                Ok(None)
            }
            "restoreTasks" => {
                let backup = harness.tasks_file.with_extension("json.bak");
                fs::remove_dir(&harness.tasks_file)
                    .unwrap_or_else(|error| panic!("#2295 gold {name}: restore: {error}"));
                fs::rename(&backup, &harness.tasks_file)
                    .unwrap_or_else(|error| panic!("#2295 gold {name}: restore: {error}"));
                Ok(None)
            }
            "writeTasks" => {
                fs::write(&harness.tasks_file, text(action, "text"))
                    .unwrap_or_else(|error| panic!("#2295 gold {name}: write: {error}"));
                Ok(None)
            }
            "observe" => {
                observations.push((text(action, "as"), wait_settled(&harness)));
                Ok(None)
            }
            other => panic!("#2295 gold {name} action {index}: unknown kind {other}"),
        };
        let ok = recorded
            .get("ok")
            .and_then(JsValue::as_bool)
            .unwrap_or_else(|| panic!("#2295 gold {name} action {index}: ok must be boolean"));
        match (outcome, ok) {
            (Ok(None), true) => assert_eq!(
                recorded.get("json").and_then(JsValue::as_string),
                None,
                "#2295 gold {name} action {index} ({kind}): the TS gold recorded a value"
            ),
            (Ok(Some(value)), true) => {
                let serialized = stringify_json(&value)
                    .unwrap_or_else(|error| panic!("#2295 gold {name} action {index}: {error:?}"))
                    .unwrap_or_else(|| {
                        panic!(
                            "#2295 gold {name} action {index} ({kind}): the value must serialize"
                        )
                    });
                assert_eq!(
                    serialized,
                    text(recorded, "json"),
                    "#2295 gold {name} action {index} ({kind}) differs from the TS gold"
                );
            }
            (Err(error), false) => {
                let recorded_error = recorded.get("error").unwrap_or_else(|| {
                    panic!("#2295 gold {name} action {index}: error must be recorded")
                });
                let left = stringify_json(&error.as_json())
                    .expect("#2295 gold errors serialize")
                    .expect("#2295 gold errors are objects");
                let right = stringify_json(recorded_error)
                    .expect("#2295 gold errors serialize")
                    .expect("#2295 gold errors are objects");
                assert_eq!(
                    left, right,
                    "#2295 gold {name} action {index} ({kind}) error differs from the TS gold"
                );
            }
            (outcome, ok) => panic!(
                "#2295 gold {name} action {index} ({kind}): expected {}, got {outcome:?}",
                if ok { "a value" } else { "an error" },
            ),
        }
    }
    assert_eq!(
        advance_index,
        advance_ms.len(),
        "#2295 gold {name}: every replay sleep must map to an advance"
    );
    let Some(JsValue::Object(recorded_observations)) = cell.get("observations") else {
        panic!("#2295 gold {name}: observations must be an object");
    };
    assert_eq!(
        observations.len(),
        recorded_observations.own_entries().len(),
        "#2295 gold {name}: observation count drifted"
    );
    for (key, observed) in &observations {
        let gold = recorded_observations
            .get(key.as_str())
            .unwrap_or_else(|| panic!("#2295 gold {name}: missing observation {key}"));
        assert_eq!(
            observed.calls,
            string_array(gold.get("gatewayCalls"), "gatewayCalls"),
            "#2295 gold {name} observation {key}: gatewayCalls differ from the TS gold"
        );
        let expected_tasks = match gold.get("tasksJson") {
            Some(JsValue::Null) | None => None,
            Some(_) => Some(text(gold, "tasksJson")),
        };
        assert_eq!(
            observed.tasks_json, expected_tasks,
            "#2295 gold {name} observation {key}: tasksJson differs from the TS gold"
        );
        assert_eq!(
            observed.history,
            string_array(gold.get("runHistory"), "runHistory"),
            "#2295 gold {name} observation {key}: runHistory differs from the TS gold"
        );
        assert_eq!(
            observed.infos,
            string_array(gold.get("infos"), "infos"),
            "#2295 gold {name} observation {key}: infos differ from the TS gold"
        );
        assert_eq!(
            observed.warns,
            string_array(gold.get("warns"), "warns"),
            "#2295 gold {name} observation {key}: warns differ from the TS gold"
        );
    }
}

/// #280: the committed oracle must be what the TS generator produces.
#[test]
#[ignore = "requires fixed PDRS_BASELINE and Node v22.22.0"]
fn cron_scheduler_oracle_regenerates_identically() {
    crate::test_baseline::regenerates_identically("cron-scheduler");
}

// ------------------------------------------------- S group: scheduler gates

/// S1 + S6: `start` arms exactly `DEFAULT_IDLE_POLL_MS` and does not tick on
/// its own; `poke` re-arms at the 250ms floor (the real `deleteTask` poke
/// site, CronRuntime.ts:363).
#[test]
fn start_arms_the_idle_poll_and_poke_floors_at_250ms() {
    let harness = harness(CronManagerConfig::default(), None);
    seed_tasks(&harness, &[once_body("t1", DUE), once_body("t2", FAR)]);
    harness.manager.start().unwrap();
    let runtime = harness.manager.loaded_runtime("/fire").unwrap();
    let armed = runtime
        .bound_scheduler()
        .unwrap()
        .armed_tick_delay_ms()
        .expect("#2295 start must arm the idle poll");
    assert!(
        (DEFAULT_IDLE_POLL_MS - 1_000..=DEFAULT_IDLE_POLL_MS).contains(&armed),
        "#2295 the armed idle poll must be ~60s, got {armed}ms"
    );
    // start leaves the timer seat alone: only prepareTaskSessions' closes ran.
    assert_eq!(
        harness.gateway.calls(),
        [
            "closeSession {\"sessionKey\":\"cron:t1\",\"reason\":\"cron/unattended-policy-refresh\"}",
            "closeSession {\"sessionKey\":\"cron:t2\",\"reason\":\"cron/unattended-policy-refresh\"}",
        ],
        "#2295 start must not tick"
    );
    harness
        .manager
        .delete_task(&json("{\"taskId\": \"t2\", \"projectKey\": \"/fire\"}"))
        .unwrap();
    let poked = runtime
        .bound_scheduler()
        .unwrap()
        .armed_tick_delay_ms()
        .expect("#2295 poke must arm the floor");
    assert!(
        poked <= 250,
        "#2295 poke must arm the 250ms floor, got {poked}ms"
    );
    // The poked tick fires without any idle poll elapsing.
    wait_until("the poked tick to fire the due task", || {
        harness
            .gateway
            .calls()
            .iter()
            .any(|call| call.starts_with("submitTurn"))
            .then_some(())
    });
}

/// S2: a second runtime start is a no-op — the scheduler's running gate
/// returns before the recalculation, so the stored bytes stand.
#[test]
fn second_start_recalculates_nothing() {
    let harness = harness(CronManagerConfig::default(), Some(500));
    // A once task without nextRunAt is the one shape start's recalculation
    // rewrites (the once arm skips truthy caches); runAt stays far in the
    // future so no armed tick can fire it and rewrite the store mid-test.
    let paths = resolve_cron_paths(&harness.home.to_string_lossy(), "/fire").unwrap();
    fs::create_dir_all(&paths.project_dir).unwrap();
    fs::write(
        &paths.tasks_file,
        format!(
            "{{\"schemaVersion\":1,\"tasks\":[{}]}}\n",
            once_body("t1", FAR).replace(&format!("\"nextRunAt\":\"{FAR}\","), "")
        ),
    )
    .unwrap();
    harness.manager.start().unwrap();
    let runtime = harness.manager.loaded_runtime("/fire").unwrap();
    let after_start = fs::read_to_string(&paths.tasks_file).unwrap();
    assert!(
        after_start.contains(&format!("\"nextRunAt\": \"{FAR}\"")),
        "#2295 the first start must compute the once run: {after_start}"
    );
    runtime.start().unwrap();
    assert_eq!(
        fs::read_to_string(&paths.tasks_file).unwrap(),
        after_start,
        "#2295 the second start must not rewrite the store"
    );
    assert!(
        runtime
            .bound_scheduler()
            .unwrap()
            .armed_tick_delay_ms()
            .is_some(),
        "#2295 the scheduler stays armed across the second start"
    );
}

/// S3: after a stop, the runtime's start cannot revive the scheduler
/// (`stop`'s stopped latch is permanent, CronScheduler.ts:31-32/45).
#[test]
fn start_after_stop_does_not_revive() {
    let harness = harness(CronManagerConfig::default(), Some(100));
    seed_tasks(&harness, &[once_body("t1", DUE)]);
    harness.manager.start().unwrap();
    let runtime = harness.manager.loaded_runtime("/fire").unwrap();
    // A tick that legitimately fired while the test thread was preempted past
    // the 100ms poll is not the leak — only submits after the stop are.
    let submits_before_stop = harness
        .gateway
        .calls()
        .iter()
        .filter(|call| call.starts_with("submitTurn"))
        .count();
    harness.manager.stop().unwrap();
    runtime.start().unwrap();
    assert!(
        runtime
            .bound_scheduler()
            .unwrap()
            .armed_tick_delay_ms()
            .is_none(),
        "#2295 the stopped scheduler must not re-arm"
    );
    thread::sleep(Duration::from_millis(300));
    let submits = harness
        .gateway
        .calls()
        .iter()
        .filter(|call| call.starts_with("submitTurn"))
        .count();
    assert_eq!(
        submits, submits_before_stop,
        "#2295 nothing may fire after the restart attempt: {} submits before the stop, {} after",
        submits_before_stop, submits
    );
}

/// S4: a disabled config starts nothing.
#[test]
fn disabled_config_never_arms() {
    let harness = harness(
        CronManagerConfig {
            enabled: false,
            ..CronManagerConfig::default()
        },
        Some(100),
    );
    seed_tasks(&harness, &[once_body("t1", DUE)]);
    harness.manager.start().unwrap();
    assert!(
        harness.manager.loaded_runtime("/fire").is_none(),
        "#2295 a disabled manager must not load runtimes"
    );
    thread::sleep(Duration::from_millis(250));
    assert!(
        harness.gateway.calls().is_empty(),
        "#2295 a disabled manager must not touch the gateway"
    );
}

/// S5: start before bind throws the bind-gate error (CronManager.ts:87-89).
#[test]
fn start_before_bind_throws() {
    let scratch = Scratch::new("cron-scheduler");
    let home = scratch.path().join("home");
    fs::create_dir_all(&home).unwrap();
    let manager = CronManager::new(CronManagerOptions {
        config: CronManagerConfig::default(),
        pilot_home: home.to_string_lossy().into_owned(),
        session_overrides: None,
        now: Some(Arc::new(|| PINNED_NOW_MS)),
        uuid: None,
        logger: None,
        scheduler_idle_poll_ms: None,
    })
    .unwrap();
    let error = manager.start().unwrap_err();
    assert_eq!(
        error.message().to_utf8().expect("#2295 messages are UTF-8"),
        "CronManager.start called before bindAgentGateway."
    );
}

/// S7: `poke`'s three gates — before start and after stop the scheduler is
/// not running, so the poke is a no-op (CronScheduler.ts:56). Loading the
/// runtime without starting the manager keeps the scheduler un-started.
#[test]
fn poke_gates_on_running_and_stopped() {
    let before_start = harness(CronManagerConfig::default(), Some(500));
    seed_tasks(&before_start, &[once_body("t1", DUE)]);
    before_start
        .manager
        .list_tasks(&json("{\"projectKey\": \"/fire\"}"))
        .unwrap();
    let scheduler = before_start
        .manager
        .loaded_runtime("/fire")
        .unwrap()
        .bound_scheduler()
        .unwrap();
    scheduler.poke();
    assert!(
        scheduler.armed_tick_delay_ms().is_none(),
        "#2295 poke before start must not arm"
    );

    let after_start = harness(CronManagerConfig::default(), Some(500));
    seed_tasks(&after_start, &[once_body("t1", DUE)]);
    after_start.manager.start().unwrap();
    let scheduler = after_start
        .manager
        .loaded_runtime("/fire")
        .unwrap()
        .bound_scheduler()
        .unwrap();
    assert!(
        scheduler.armed_tick_delay_ms().is_some(),
        "#2295 start arms the idle poll"
    );
    after_start.manager.stop().unwrap();
    scheduler.poke();
    assert!(
        scheduler.armed_tick_delay_ms().is_none(),
        "#2295 poke after stop must not arm"
    );
}

/// S9/S10: stop is authoritative — with a tight poll nothing fires after
/// `stop()` returns (the TS leak that can run one post-stop tick is the
/// registered divergence; the port's join is the stricter shape). A run the
/// pre-stop window started may still write its terminal record after the
/// stop returns (TS's fire-and-forget finally), so the record count may not
/// exceed the submits that preceded the stop.
#[test]
fn stop_is_authoritative_no_post_stop_tick() {
    let harness = harness(CronManagerConfig::default(), Some(100));
    seed_tasks(&harness, &[once_body("t1", DUE)]);
    harness.manager.start().unwrap();
    let submits_before_stop = harness
        .gateway
        .calls()
        .iter()
        .filter(|call| call.starts_with("submitTurn"))
        .count();
    harness.manager.stop().unwrap();
    thread::sleep(Duration::from_millis(400));
    let observed = harness.observe();
    let submits = observed
        .calls
        .iter()
        .filter(|call| call.starts_with("submitTurn"))
        .count();
    assert_eq!(
        submits, submits_before_stop,
        "#2295 no tick may run after stop: {} submits before, {} after: {:?}",
        submits_before_stop, submits, observed.calls
    );
    assert!(
        observed.history.len() <= submits_before_stop,
        "#2295 no run record may appear without a pre-stop submit: {:?}",
        observed.history
    );
}

/// The pre-register window of #2295 fix-r1: the tick pre-registers the run
/// before spawning the fire thread, and the fire re-registers it; a stop
/// that lands in that window must survive the re-register (TS registers once
/// in the calling thread before the fire's first await, so the flag can
/// never be lost there). The now seam parks the fire thread before its
/// register, forcing the interleaving deterministically.
#[test]
fn stop_in_the_pre_register_window_survives_the_re_register() {
    let release = Arc::new((Mutex::new(false), Condvar::new()));
    let owner = thread::current().id();
    let now = {
        let release = release.clone();
        Arc::new(move || {
            // Only calls off the arming thread park: whether the fire thread
            // races ahead or lags, its re-register always happens after the
            // stop below has completed.
            if thread::current().id() != owner {
                let (locked, cvar) = &*release;
                let mut open = locked.lock().unwrap();
                while !*open {
                    open = cvar.wait(open).unwrap();
                }
            }
            PINNED_NOW_MS
        })
    };
    let harness = harness_with_now(CronManagerConfig::default(), Some(10_000), now);
    seed_tasks(&harness, &[cron_body("t1", DUE)]);
    // The tick pre-registers run id-1 and spawns the fire thread, which
    // parks in the gated now before its own register.
    harness.manager.run_tick_once(Some("/fire")).unwrap();
    let runtime = harness.manager.loaded_runtime("/fire").unwrap();
    assert_eq!(
        runtime.active_run_count(),
        1,
        "#2295 the tick must pre-register the run before the fire thread starts"
    );
    harness
        .manager
        .stop_task(&json("{\"taskId\": \"t1\", \"projectKey\": \"/fire\"}"))
        .unwrap();
    // Open the gate: the fire's re-register merges, keeping the requested
    // stop, so the finally records "stopped" and the cron arm still
    // reschedules.
    {
        let (locked, cvar) = &*release;
        *locked.lock().unwrap() = true;
        cvar.notify_all();
    }
    let observed = wait_settled(&harness);
    assert_eq!(
        observed.history.len(),
        1,
        "#2295 the pre-registered run must still write its terminal record: {:?}",
        observed.history
    );
    let record = json(&observed.history[0]);
    assert_eq!(
        text(&record, "outcome"),
        "stopped",
        "#2295 a stop requested in the pre-register window must survive the fire's re-register"
    );
    assert!(
        observed
            .calls
            .iter()
            .any(|call| call.starts_with("submitTurn")),
        "#2295 the stopped run still submits its turn (TS marks, not cancels)"
    );
}

/// #2295 fix-r1: a panicking tick seam must not kill the scheduler thread —
/// TS folds a rejecting `tick()` into the "cron scheduler tick failed" warn
/// and the `.then` still re-arms (CronScheduler.ts:77-86), so the next tick
/// still fires the due task.
#[test]
fn tick_panic_folds_into_a_warn_and_re_arms() {
    let panics = Arc::new(AtomicUsize::new(0));
    let now = {
        let panics = panics.clone();
        Arc::new(move || {
            // Only the scheduler thread's first read panics: start()'s own
            // recalculation reads the seam on the caller's thread, and the
            // fire thread ("cron-fire") must stay clean.
            if thread::current()
                .name()
                .is_some_and(|name| name == "cron-scheduler")
                && panics.fetch_add(1, Ordering::SeqCst) == 0
            {
                panic!("#2295 the seam throws mid-tick");
            }
            PINNED_NOW_MS
        })
    };
    let harness = harness_with_now(CronManagerConfig::default(), Some(30), now);
    seed_tasks(&harness, &[once_body("t1", DUE)]);
    harness.manager.start().unwrap();
    wait_until("the panicked tick to fold into a warn", || {
        harness
            .logger
            .warns()
            .iter()
            .any(|warn| warn.contains("cron scheduler tick failed"))
            .then_some(())
    });
    // fix-r2's MIN_TIMER_MS floor puts a 250ms gap between the folded tick
    // and the re-arm — longer than the settle window, so the fire is awaited
    // explicitly instead of relying on the settle pump to observe it.
    wait_until("the post-panic tick to fire the due task", || {
        harness
            .gateway
            .calls()
            .iter()
            .any(|call| call.starts_with("submitTurn"))
            .then_some(())
    });
    let observed = wait_settled(&harness);
    assert!(
        observed
            .calls
            .iter()
            .any(|call| call.starts_with("submitTurn")),
        "#2295 the post-panic tick must still fire the due task: {:?}",
        observed.calls
    );
    assert_eq!(
        observed.history.len(),
        1,
        "#2295 the fired run must still complete after the panic: {:?}",
        observed.history
    );
}

/// #2295 fix-r2: a start whose recalculation fails leaves exactly the TS
/// state — `running` already set, no timer armed (CronScheduler.ts:38-40, the
/// reject keeps the latches) — so the second start is the running-gate no-op
/// (the still-broken store is not re-read), and a later `poke()` revives the
/// scheduler: the poked 250ms tick fires the due task (CronScheduler.ts:
/// 55-64). The broken store is the gold tick-failure shape (EISDIR); a
/// recording fire port stands in for the turn, and the manager-loaded runtime
/// keeps the seat's weak upgrade alive.
#[test]
fn poke_after_a_failed_start_recalculation_fires_the_due_task() {
    let harness = harness(CronManagerConfig::default(), Some(10_000));
    seed_tasks(&harness, &[once_body("t1", DUE)]);
    harness
        .manager
        .list_tasks(&json("{\"projectKey\": \"/fire\"}"))
        .unwrap();
    let runtime = harness.manager.loaded_runtime("/fire").unwrap();
    let fired: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    struct RecordingFire(Arc<Mutex<Vec<String>>>);
    impl CronFirePort for RecordingFire {
        fn run_task(&self, task: &JsValue, _run_id: &JsString) -> Result<(), CronError> {
            self.0.lock().unwrap().push(
                task.get("taskId")
                    .and_then(JsValue::as_string)
                    .unwrap_or_else(|| panic!("#2295 the fired task carries a taskId"))
                    .to_utf8()
                    .expect("#2295 task ids are UTF-8")
                    .to_string(),
            );
            Ok(())
        }
    }
    let paths = resolve_cron_paths(&harness.home.to_string_lossy(), "/fire").unwrap();
    let scheduler = CronScheduler::new(SchedulerDeps {
        config: CronManagerConfig::default(),
        store: CronTaskStore::new(paths),
        fire: Arc::new(RecordingFire(fired.clone())),
        uuid: Arc::new(|| JsString::from("id-1")),
        now: Arc::new(|| PINNED_NOW_MS),
        active_run_count: Arc::new(|| 0),
        logger: Arc::new(MessageLogger::default()),
        zone_cache: Arc::new(CronZoneResolverCache::new()),
        // Far enough that the seat's own re-arm never races the poked tick.
        idle_poll_ms: 10_000,
        runtime: Arc::downgrade(&runtime),
    })
    .unwrap();
    // Break the store: start's recalculation fails with running latched.
    let backup = harness.tasks_file.with_extension("json.bak");
    fs::rename(&harness.tasks_file, &backup).unwrap();
    fs::create_dir(&harness.tasks_file).unwrap();
    scheduler
        .start()
        .expect_err("#2295 the broken store must fail the start recalculation");
    // The second start is the running-gate no-op: the store is still broken,
    // so a re-run recalculation would fail again — running must have stayed
    // set through the rejection.
    scheduler.start().expect(
        "#2295 the running gate must make the second start a no-op despite the broken store",
    );
    assert!(
        scheduler.armed_tick_delay_ms().is_none(),
        "#2295 a failed start must leave nothing armed (the TS timer stays undefined)"
    );
    // Restore and revive: the poked 250ms tick fires the due task.
    fs::remove_dir(&harness.tasks_file).unwrap();
    fs::rename(&backup, &harness.tasks_file).unwrap();
    scheduler.poke();
    let armed = scheduler
        .armed_tick_delay_ms()
        .expect("#2295 poke must arm the floor after the failed start");
    assert!(
        armed <= 250,
        "#2295 the poked floor must arm at 250ms after the failed start, got {armed}ms"
    );
    let fired_tasks = wait_until(
        "the poked tick to fire the due task after the failed start",
        || {
            let fired = fired.lock().unwrap();
            (!fired.is_empty()).then(|| fired.clone())
        },
    );
    assert_eq!(
        fired_tasks,
        vec!["t1".to_owned()],
        "#2295 the revived tick must fire exactly the due task"
    );
}

/// #2295 fix-r2: every arm floors at `MIN_TIMER_MS` — `scheduleNextTick`'s
/// `Math.max(MIN_TIMER_MS, delayMs ?? computeDelayMs())` (CronScheduler.ts:
/// 73): an injected idle poll of 0 must not arm at "now" (a deadline that
/// `checked_duration_since` already sees as due would hot-loop `listTasks`).
/// The now seam counts every tick's read, so a hot loop shows up as an
/// unbounded call count.
#[test]
fn idle_poll_below_the_floor_arms_at_the_floor() {
    let ticks = Arc::new(AtomicUsize::new(0));
    let now = {
        let ticks = ticks.clone();
        Arc::new(move || {
            ticks.fetch_add(1, Ordering::SeqCst);
            PINNED_NOW_MS
        })
    };
    let harness = harness_with_now(CronManagerConfig::default(), Some(0), now);
    seed_tasks(&harness, &[once_body("t1", FAR)]);
    harness.manager.start().unwrap();
    let scheduler = harness
        .manager
        .loaded_runtime("/fire")
        .unwrap()
        .bound_scheduler()
        .unwrap();
    let armed = scheduler
        .armed_tick_delay_ms()
        .expect("#2295 start must arm the floored idle poll");
    assert!(
        (100..=250).contains(&armed),
        "#2295 an idle poll below the floor must arm at the 250ms floor, got {armed}ms"
    );
    // The re-arm after a tick floors too: in 700ms the floored seat ticks a
    // handful of times (plus start's recalculation and lifecycle reads), not
    // the unbounded cadence of an unfloored 0ms poll.
    thread::sleep(Duration::from_millis(700));
    let calls = ticks.load(Ordering::SeqCst);
    assert!(
        calls <= 12,
        "#2295 the floored idle poll must not hot-loop: {calls} now-seam calls in 700ms"
    );
}

/// #2295 fix-r2: a huge injected idle poll must not panic the arm — std
/// documents `Instant + Duration` as "may panic" on overflow, so the port arms
/// through `checked_add`. The seat arms at the far-future deadline and stays
/// poke-alive: the poked 250ms tick fires the due task, and the tick's own
/// re-arm at the huge poll keeps the seat armed.
#[test]
fn huge_idle_poll_arms_without_panicking_and_stays_poke_alive() {
    let harness = harness(CronManagerConfig::default(), Some(u64::MAX));
    seed_tasks(&harness, &[once_body("t1", DUE)]);
    harness.manager.start().unwrap();
    let scheduler = harness
        .manager
        .loaded_runtime("/fire")
        .unwrap()
        .bound_scheduler()
        .unwrap();
    let armed = scheduler
        .armed_tick_delay_ms()
        .expect("#2295 the huge idle poll must arm at a representable deadline");
    assert!(
        armed > DEFAULT_IDLE_POLL_MS,
        "#2295 the huge idle poll must arm at the far-future deadline, got {armed}ms"
    );
    scheduler.poke();
    wait_until(
        "the poked tick to fire the due task under the huge idle poll",
        || {
            harness
                .gateway
                .calls()
                .iter()
                .any(|call| call.starts_with("submitTurn"))
                .then_some(())
        },
    );
    let observed = wait_settled(&harness);
    assert_eq!(
        observed.history.len(),
        1,
        "#2295 the fired run must complete under the huge idle poll: {:?}",
        observed.history
    );
    assert!(
        scheduler.armed_tick_delay_ms().is_some(),
        "#2295 the tick's re-arm at the huge idle poll must keep the seat armed"
    );
}

/// #2295 fix-r1 arbitration: two fire threads drive turns on the gateway's
/// current-thread runtime through `Handle::block_on` — the exact production
/// topology of `NativeCronGatewayBody::run_turn` (assembly/cron.rs) — while
/// this thread is the runtime's driver. Both parked timer turns must
/// complete: two std threads blocking on the same current-thread runtime
/// must not hang or starve its timers.
#[test]
fn two_fire_threads_drive_the_shared_current_thread_runtime() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("#2295 the current-thread runtime must build");
    let handle = runtime.handle().clone();
    let harness = harness(
        CronManagerConfig {
            max_concurrent_runs: 2.0,
            ..CronManagerConfig::default()
        },
        Some(50),
    );
    seed_tasks(&harness, &[once_body("t1", DUE), once_body("t2", DUE)]);
    harness.gateway.set_submit(move |_| {
        handle.block_on(async {
            tokio::time::sleep(Duration::from_millis(60)).await;
            Ok(())
        })
    });
    harness.manager.start().unwrap();
    // This thread is the runtime's driver — the production main loop's seat.
    runtime.block_on(async {
        tokio::time::sleep(Duration::from_millis(400)).await;
    });
    let observed = wait_settled(&harness);
    let submits = observed
        .calls
        .iter()
        .filter(|call| call.starts_with("submitTurn"))
        .count();
    assert_eq!(
        submits, 2,
        "#2295 both due tasks must fire under max_concurrent_runs=2: {:?}",
        observed.calls
    );
    assert_eq!(
        observed.history.len(),
        2,
        "#2295 both parked turns must complete on the shared runtime: {:?}",
        observed.history
    );
}

/// S12: `runTickOnce` is public and un-gated — it still ticks after a stop
/// (CronScheduler.ts:66-69).
#[test]
fn run_tick_once_after_stop_still_ticks() {
    let harness = harness(CronManagerConfig::default(), Some(10_000));
    seed_tasks(&harness, &[once_body("t1", DUE)]);
    harness.manager.start().unwrap();
    let runtime = harness.manager.loaded_runtime("/fire").unwrap();
    harness.manager.stop().unwrap();
    runtime.run_tick_once().unwrap();
    let observed = wait_settled(&harness);
    assert!(
        observed
            .calls
            .iter()
            .any(|call| call.starts_with("submitTurn")),
        "#2295 run_tick_once must tick despite the stopped scheduler"
    );
}

/// S8's port half: a poke's 250ms tick runs while another run is parked
/// mid-turn, and the in-flight run (registered by the tick, before the fire
/// thread even starts) counts toward the concurrency limit across threads —
/// the second due task is deferred by the constant 60s, never fired.
#[test]
fn poke_tick_gates_on_the_in_flight_run() {
    let harness = harness(CronManagerConfig::default(), Some(10_000));
    seed_tasks(&harness, &[once_body("t1", DUE), once_body("t2", FAR)]);
    // The parked turn: submit blocks until the test releases it.
    let (release, gate) = mpsc::channel::<()>();
    let parked = Mutex::new(gate);
    harness.gateway.set_submit(move |_| {
        (*parked.lock().unwrap())
            .recv()
            .map_err(|_| CronError::error("the parked turn was released by a dropped channel"))
    });
    harness.manager.start().unwrap();
    let runtime = harness.manager.loaded_runtime("/fire").unwrap();
    runtime.run_tick_once().unwrap();
    wait_until("the first submit to park", || {
        harness
            .gateway
            .calls()
            .iter()
            .any(|call| call.starts_with("submitTurn"))
            .then_some(())
    });
    // A second due task behind the store's back — t1's own running state
    // hand-written so its post-run matcher still finds it — plus the delete
    // poke site that wakes the 250ms tick.
    let running_t1 = once_body("t1", DUE)
        .replace("\"status\":\"scheduled\"", "\"status\":\"running\"")
        .replace("\"revision\":0", "\"revision\":1,\"lastRunId\":\"id-1\"");
    fs::write(
        &harness.tasks_file,
        format!(
            "{{\"schemaVersion\":1,\"tasks\":[{},{},{}]}}\n",
            running_t1,
            once_body("t2", FAR),
            once_body("t3", DUE)
        ),
    )
    .unwrap();
    harness
        .manager
        .delete_task(&json("{\"taskId\": \"t2\", \"projectKey\": \"/fire\"}"))
        .unwrap();
    let delayed = wait_until("the poked tick to defer t3 by the constant", || {
        let tasks = fs::read_to_string(&harness.tasks_file).ok()?;
        tasks
            .contains("\"nextRunAt\": \"2026-01-01T00:01:00.000Z\"")
            .then_some(tasks)
    });
    assert!(
        delayed.contains("\"taskId\": \"t3\""),
        "#2295 the deferred task stands deferred, not fired: {delayed}"
    );
    let submits = harness
        .gateway
        .calls()
        .iter()
        .filter(|call| call.starts_with("submitTurn"))
        .count();
    assert_eq!(
        submits, 1,
        "#2295 the in-flight run must gate the poked tick's capacity check"
    );
    // Releasing the parked run lets its finally complete: record, delivery,
    // once delete, session release.
    release.send(()).unwrap();
    let observed = wait_settled(&harness);
    assert_eq!(
        observed.history.len(),
        1,
        "#2295 only the parked run completes: {:?}",
        observed.history
    );
    assert!(
        !observed
            .tasks_json
            .as_deref()
            .is_some_and(|raw| raw.contains("\"t1\"")),
        "#2295 the parked once task is deleted after its release: {:?}",
        observed.tasks_json
    );
    assert!(
        observed
            .tasks_json
            .as_deref()
            .is_some_and(|raw| raw.contains("\"t3\"")),
        "#2295 the deferred task stands deferred: {:?}",
        observed.tasks_json
    );
}

/// T3: a running task is never due, even with a past nextRunAt
/// (CronScheduler.ts:191-193).
#[test]
fn running_task_is_not_due() {
    let harness = harness(CronManagerConfig::default(), None);
    seed_tasks(
        &harness,
        &[once_body("t1", DUE).replace("\"status\":\"scheduled\"", "\"status\":\"running\"")],
    );
    harness
        .manager
        .list_tasks(&json("{\"projectKey\": \"/fire\"}"))
        .unwrap();
    harness
        .manager
        .loaded_runtime("/fire")
        .unwrap()
        .run_tick_once()
        .unwrap();
    assert!(
        harness
            .gateway
            .calls()
            .iter()
            .all(|call| !call.starts_with("submitTurn")),
        "#2295 a running task must not re-fire"
    );
    assert!(
        harness.observe().history.is_empty(),
        "#2295 no run record may appear"
    );
}

/// T4: a due task over the concurrency limit is deferred by the constant
/// 60s — `delayTask` writes the constant, not the injected idle poll
/// (CronScheduler.ts:97-99,176-187).
#[test]
fn capacity_defers_by_the_constant_sixty_seconds() {
    let scratch = Scratch::new("cron-scheduler");
    let home = scratch.path().join("home");
    fs::create_dir_all(&home).unwrap();
    let paths = resolve_cron_paths(&home.to_string_lossy(), "/fire").unwrap();
    fs::create_dir_all(&paths.project_dir).unwrap();
    fs::write(
        &paths.tasks_file,
        format!(
            "{{\"schemaVersion\":1,\"tasks\":[{}]}}\n",
            once_body("t1", DUE)
        ),
    )
    .unwrap();
    let fired = Arc::new(AtomicBool::new(false));
    struct NoFire(Arc<AtomicBool>);
    impl CronFirePort for NoFire {
        fn run_task(&self, _task: &JsValue, _run_id: &JsString) -> Result<(), CronError> {
            self.0.store(true, Ordering::SeqCst);
            Ok(())
        }
    }
    let scheduler = CronScheduler::new(SchedulerDeps {
        config: CronManagerConfig::default(),
        store: CronTaskStore::new(paths.clone()),
        fire: Arc::new(NoFire(fired.clone())),
        uuid: Arc::new(|| JsString::from("id-1")),
        now: Arc::new(|| PINNED_NOW_MS),
        active_run_count: Arc::new(|| 1),
        logger: Arc::new(MessageLogger::default()),
        zone_cache: Arc::new(CronZoneResolverCache::new()),
        idle_poll_ms: 50,
        // The capacity path returns before the fire spawn, so it never
        // upgrades the runtime — a dangling weak stands in.
        runtime: Weak::new(),
    })
    .unwrap();
    scheduler.run_tick_once().unwrap();
    assert!(
        !fired.load(Ordering::SeqCst),
        "#2295 a task over the limit must not fire"
    );
    let tasks = json(&fs::read_to_string(&paths.tasks_file).unwrap());
    let task = oracle_array(tasks.get("tasks"), "tasks")
        .into_iter()
        .next()
        .expect("the deferred task stands");
    assert_eq!(text(&task, "nextRunAt"), "2026-01-01T00:01:00.000Z");
    assert_eq!(text(&task, "status"), "scheduled");
    assert_eq!(
        task.get("revision").and_then(JsValue::as_number),
        Some(1.0),
        "#2295 the deferral bumps the revision"
    );
}

// The direct-CronFire cells: one scratch store, the injectable registry and
// delivery seams, and the gateway's configurable submit body.
struct FireHarness {
    fire: CronFire,
    home: PathBuf,
    paths: super::CronPaths,
    gateway: Arc<Gateway>,
    logger: Arc<MessageLogger>,
    registry: Arc<Mutex<Vec<CronActiveRun>>>,
    releases: Arc<Mutex<Vec<String>>>,
    deliveries: Arc<Mutex<Vec<String>>>,
    _scratch: Scratch,
}

fn fire_harness(delivery_fails: bool) -> FireHarness {
    let scratch = Scratch::new("cron-scheduler");
    let home = scratch.path().join("home");
    fs::create_dir_all(&home).unwrap();
    let paths = resolve_cron_paths(&home.to_string_lossy(), "/fire").unwrap();
    let gateway = Gateway::new();
    let logger = Arc::new(MessageLogger::default());
    let registry: Arc<Mutex<Vec<CronActiveRun>>> = Arc::new(Mutex::new(Vec::new()));
    let releases: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let deliveries: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let register = {
        let registry = registry.clone();
        // The manager's register semantics (manager.rs): a same-runId
        // re-register merges, keeping a `stop_requested` set in the
        // pre-register window.
        Arc::new(move |run: CronActiveRun| {
            let mut runs = registry.lock().unwrap();
            match runs
                .iter_mut()
                .find(|existing| existing.run_id == run.run_id)
            {
                Some(existing) => {
                    let stop_requested = existing.stop_requested || run.stop_requested;
                    *existing = run;
                    existing.stop_requested = stop_requested;
                }
                None => runs.push(run),
            }
        }) as Arc<dyn Fn(CronActiveRun) + Send + Sync>
    };
    let unregister = {
        let registry = registry.clone();
        Arc::new(move |run_id: &JsString| {
            let mut runs = registry.lock().unwrap();
            runs.iter()
                .position(|run| &run.run_id == run_id)
                .map(|index| runs.remove(index))
        }) as Arc<dyn Fn(&JsString) -> Option<CronActiveRun> + Send + Sync>
    };
    let get_active_run = {
        let registry = registry.clone();
        Arc::new(move |run_id: &JsString| {
            registry
                .lock()
                .unwrap()
                .iter()
                .find(|run| &run.run_id == run_id)
                .cloned()
        }) as Arc<dyn Fn(&JsString) -> Option<CronActiveRun> + Send + Sync>
    };
    let release_task_session = {
        let releases = releases.clone();
        Arc::new(move |task: &JsValue| {
            let session = task
                .get("sessionKey")
                .and_then(JsValue::as_string)
                .map(|key| key.to_utf8().expect("#2295 session keys are UTF-8"))
                .unwrap_or_default();
            releases.lock().unwrap().push(session);
        }) as Arc<dyn Fn(&JsValue) + Send + Sync>
    };
    let on_result_delivery = if delivery_fails {
        Some(
            Arc::new(|_delivery: &JsValue| Err(CronError::error("delivery refused")))
                as Arc<dyn Fn(&JsValue) -> Result<(), CronError> + Send + Sync>,
        )
    } else {
        let deliveries = deliveries.clone();
        Some(Arc::new(move |delivery: &JsValue| {
            deliveries.lock().unwrap().push(
                stringify_json(delivery)
                    .expect("#2295 deliveries serialize")
                    .expect("#2295 deliveries are objects"),
            );
            Ok(())
        })
            as Arc<
                dyn Fn(&JsValue) -> Result<(), CronError> + Send + Sync,
            >)
    };
    let fire = CronFire::new(CronFireDeps {
        gateway: gateway.clone(),
        store: CronTaskStore::new(paths.clone()),
        now: Arc::new(|| PINNED_NOW_MS),
        register_active_run: register,
        unregister_active_run: unregister,
        get_active_run,
        run_timeout_ms: 3_600_000.0,
        default_timezone: "UTC".into(),
        release_task_session,
        on_result_delivery,
        logger: logger.clone(),
        zone_cache: Arc::new(CronZoneResolverCache::new()),
    });
    FireHarness {
        fire,
        home,
        paths,
        gateway,
        logger,
        registry,
        releases,
        deliveries,
        _scratch: scratch,
    }
}

impl FireHarness {
    fn seed(&self, bodies: &[String]) {
        let paths = resolve_cron_paths(&self.home.to_string_lossy(), "/fire").unwrap();
        fs::create_dir_all(&paths.project_dir).unwrap();
        fs::write(
            &paths.tasks_file,
            format!("{{\"schemaVersion\":1,\"tasks\":[{}]}}\n", bodies.join(",")),
        )
        .unwrap();
    }

    fn snapshot(&self) -> JsValue {
        self.store_list()
            .into_iter()
            .next()
            .expect("#2295 one seeded task")
    }

    fn store_list(&self) -> Vec<JsValue> {
        CronTaskStore::new(self.paths.clone())
            .list_tasks()
            .expect("#2295 the store must list")
    }

    fn history(&self) -> Vec<JsValue> {
        fs::read_to_string(&self.paths.run_history_file)
            .unwrap_or_default()
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(json)
            .collect()
    }

    fn tasks_text(&self) -> String {
        fs::read_to_string(&self.paths.tasks_file).unwrap_or_default()
    }
}

/// T5: a claim against a stale snapshot aborts — no turn, no record, no
/// post-run update (CronFire.ts:82-85).
#[test]
fn stale_claim_aborts_without_record() {
    let harness = fire_harness(false);
    harness.seed(&[cron_body("t1", DUE)]);
    let mut stale = harness.snapshot();
    let JsValue::Object(task) = &mut stale else {
        panic!("#2295 the store returns an object task");
    };
    task.insert("revision".into(), JsValue::Number(5.0));
    harness
        .fire
        .run_task(&stale, &JsString::from("run-stale"))
        .unwrap();
    assert!(
        harness.gateway.calls().is_empty(),
        "#2295 a stale claim must not submit a turn"
    );
    assert!(harness.history().is_empty(), "#2295 no run record");
    assert!(
        harness.registry.lock().unwrap().is_empty(),
        "#2295 the aborted run must unregister"
    );
    assert_eq!(text(&harness.snapshot(), "status"), "scheduled");
}

/// The classification ladder over the synchronous submit boundary
/// (CronFire.ts:125-140 folded): each terminal error event code reaches the
/// run record with its mapped outcome, and the cron arm still reschedules.
#[test]
fn turn_errors_classify_by_code() {
    for (code, outcome, recorded_code) in [
        ("turn_timeout", "failed", "cron_run_timeout"),
        ("agent_aborted", "aborted", "agent_aborted"),
        ("session_busy", "failed", "session_busy"),
    ] {
        let harness = fire_harness(false);
        harness.seed(&[cron_body("t1", DUE)]);
        let error = CronError::gateway(code, JsString::from(format!("the {code} event")));
        harness.gateway.set_submit(move |_| Err(error.clone()));
        harness
            .fire
            .run_task(&harness.snapshot(), &JsString::from("run-1"))
            .unwrap();
        let history = harness.history();
        assert_eq!(history.len(), 1, "#2295 {code}: one terminal record");
        assert_eq!(text(&history[0], "outcome"), outcome, "#2295 {code}");
        assert_eq!(
            text(history[0].get("error").unwrap(), "code"),
            recorded_code,
            "#2295 {code}"
        );
        // The cron arm still reschedules regardless of the outcome.
        let task = harness.snapshot();
        assert_eq!(text(&task, "status"), "scheduled", "#2295 {code}");
        assert_eq!(
            text(&task, "nextRunAt"),
            "2026-01-01T00:05:00.000Z",
            "#2295 {code}"
        );
        assert!(
            harness.logger.warns().is_empty(),
            "#2295 {code}: a classified error is not a warn: {:?}",
            harness.logger.warns()
        );
    }
}

/// T11: a code-less rejection is the thrown-seam fold — `cron_run_failed`
/// with the raw message (CronFire.ts:142-149).
#[test]
fn codeless_rejection_folds_to_cron_run_failed() {
    let harness = fire_harness(false);
    harness.seed(&[cron_body("t1", DUE)]);
    harness
        .gateway
        .set_submit(|_| Err(CronError::error("the seam threw")));
    harness
        .fire
        .run_task(&harness.snapshot(), &JsString::from("run-1"))
        .unwrap();
    let history = harness.history();
    assert_eq!(text(&history[0], "outcome"), "failed");
    let error = history[0].get("error").unwrap();
    assert_eq!(text(error, "code"), "cron_run_failed");
    assert_eq!(text(error, "message"), "the seam threw");
    // The failed delivery text is the error message, not the fallback.
    let deliveries = harness.deliveries.lock().unwrap().clone();
    assert_eq!(deliveries.len(), 1);
    assert!(
        deliveries[0].contains("\"text\":\"the seam threw\""),
        "#2295 the failed delivery carries the error text: {deliveries:?}"
    );
}

/// T12: a stop requested during the turn overrides the outcome, and the
/// post-run update still runs (CronFire.ts:151-154).
#[test]
fn stop_requested_overrides_the_outcome() {
    let harness = fire_harness(false);
    harness.seed(&[once_body("t1", DUE)]);
    let registry = harness.registry.clone();
    harness.gateway.set_submit(move |input| {
        registry
            .lock()
            .unwrap()
            .iter_mut()
            .find(|run| run.run_id == input.run_id)
            .expect("#2295 the run registers before the submit")
            .stop_requested = true;
        Ok(())
    });
    harness
        .fire
        .run_task(&harness.snapshot(), &JsString::from("run-1"))
        .unwrap();
    let history = harness.history();
    assert_eq!(text(&history[0], "outcome"), "stopped");
    // The once arm still deletes and releases.
    assert!(
        !harness.tasks_text().contains("\"taskId\""),
        "#2295 the stopped once task is still deleted"
    );
    assert_eq!(
        *harness.releases.lock().unwrap(),
        ["cron:t1".to_owned()],
        "#2295 the session releases after the stopped run"
    );
}

/// #2295 fix-r1: a panicking submit seam must not leak the registered run —
/// TS's `finally` runs `unregisterActiveRun` even when the try block throws
/// (CronFire.ts:150-202), so the port's panic path unregisters too (a
/// residue would keep the run counted against the capacity gate forever and
/// defer every later cron task).
#[test]
fn submit_panic_still_unregisters() {
    let harness = fire_harness(false);
    harness.seed(&[cron_body("t1", DUE)]);
    harness
        .gateway
        .set_submit(|_| panic!("#2295 the submit seam throws"));
    let error = harness
        .fire
        .run_task(&harness.snapshot(), &JsString::from("run-1"))
        .expect_err("#2295 the panic must surface as the port's Err");
    assert_eq!(
        error.message().to_utf8().expect("#2295 messages are UTF-8"),
        "#2295 the submit seam throws",
        "#2295 the panic payload must ride the Err"
    );
    assert!(
        harness.registry.lock().unwrap().is_empty(),
        "#2295 a panicking submit seam must not leak the active run"
    );
}

/// T13: a failed terminal record only warns — the post-run update still runs
/// (CronFire.ts:172-177).
#[test]
fn append_run_failure_warns_and_continues() {
    let harness = fire_harness(false);
    harness.seed(&[once_body("t1", DUE)]);
    fs::create_dir(&harness.paths.run_history_file)
        .expect("#2295 the record sink becomes a directory");
    harness
        .fire
        .run_task(&harness.snapshot(), &JsString::from("run-1"))
        .unwrap();
    assert_eq!(
        harness.logger.warns(),
        ["warn cron run terminal record write failed".to_owned()],
        "#2295 the record failure must warn once"
    );
    assert!(
        !harness.tasks_text().contains("\"taskId\""),
        "#2295 the post-run update still deletes the once task"
    );
}

/// T14: a rejecting delivery handler only warns (CronFire.ts:188-193).
#[test]
fn delivery_failure_warns() {
    let harness = fire_harness(true);
    harness.seed(&[once_body("t1", DUE)]);
    harness
        .fire
        .run_task(&harness.snapshot(), &JsString::from("run-1"))
        .unwrap();
    assert_eq!(
        harness.logger.warns(),
        ["warn cron result delivery failed".to_owned()]
    );
    assert!(
        harness.deliveries.lock().unwrap().is_empty(),
        "#2295 the rejecting handler never records"
    );
    assert!(
        !harness.tasks_text().contains("\"taskId\""),
        "#2295 the post-run update still runs after a failed delivery"
    );
}

/// T15: a failed post-run update only warns — the terminal record stands
/// (CronFire.ts:195-200).
#[test]
fn post_run_update_failure_warns() {
    let harness = fire_harness(false);
    harness.seed(&[once_body("t1", DUE)]);
    let tasks_file = harness.paths.tasks_file.clone();
    harness.gateway.set_submit(move |_| {
        // Break the store mid-turn: the post-run matcher can no longer run.
        fs::rename(&tasks_file, tasks_file.with_extension("json.bak"))
            .expect("#2295 break the task file mid-turn");
        fs::create_dir(&tasks_file).expect("#2295 the task file becomes a directory");
        Ok(())
    });
    harness
        .fire
        .run_task(&harness.snapshot(), &JsString::from("run-1"))
        .unwrap();
    assert_eq!(
        harness.logger.warns(),
        ["warn cron task post-run update failed".to_owned()]
    );
    assert_eq!(
        harness.history().len(),
        1,
        "#2295 the terminal record is written before the post-run update"
    );
    // The release never ran (the once arm did not complete its delete).
    assert!(
        harness.releases.lock().unwrap().is_empty(),
        "#2295 a failed post-run update must not release"
    );
}

/// T16: a concurrent revision change between the claim and the post-run
/// update leaves the task standing (CronFire.ts:288-294).
#[test]
fn stale_post_run_update_keeps_the_task() {
    let harness = fire_harness(false);
    harness.seed(&[once_body("t1", DUE)]);
    let store = CronTaskStore::new(harness.paths.clone());
    harness.gateway.set_submit(move |_| {
        store
            .update_task("t1", |current| {
                let JsValue::Object(object) = current else {
                    return Some(current.clone());
                };
                let mut updated = object.clone();
                updated.insert(
                    "revision".into(),
                    JsValue::Number(
                        current
                            .get("revision")
                            .and_then(JsValue::as_number)
                            .unwrap_or(0.0)
                            + 1.0,
                    ),
                );
                Some(JsValue::Object(updated))
            })
            .expect("#2295 the concurrent change lands");
        Ok(())
    });
    harness
        .fire
        .run_task(&harness.snapshot(), &JsString::from("run-1"))
        .unwrap();
    assert_eq!(
        harness.history().len(),
        1,
        "#2295 the terminal record still stands"
    );
    let task = harness.snapshot();
    assert_eq!(
        text(&task, "status"),
        "running",
        "#2295 the task keeps its running state"
    );
    assert_eq!(
        text(&task, "lastRunId"),
        "run-1",
        "#2295 the claim survives; only the post-run match fails"
    );
    assert!(
        harness.releases.lock().unwrap().is_empty(),
        "#2295 a stale post-run match must not release the session"
    );
}
