//! #1353 A8.5-pr2 machine tests: the matrix §二 order and §四 failure
//! semantics over injected faces, so A2/A3/A8/A9/A10 are deterministic. The
//! production wiring has its own tests (`native_process/switch_wiring_tests`);
//! A13's clean process exit is `tests/switch_e2e.rs`.
use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use pilotdeck_protocol::JsString;

use crate::runtime::cron::{
    CronActiveRun, CronAgentGateway, CronError, CronManager, CronManagerConfig, CronManagerOptions,
};
use crate::switch::{
    BACKSTOP_SLACK_MS, BoundedTimeout, Phase, StepFailure, SwitchError, SwitchFaces, SwitchMachine,
    SwitchTimeouts, TIMEOUT_CAP_MS,
};
use crate::test_scratch::Scratch;
use crate::{negative_assert, negative_assert_eq};

fn bound(ms: u64) -> BoundedTimeout {
    BoundedTimeout::from_millis(ms).expect("test bound is inside the DC-1 predicate")
}

fn uniform(ms: u64) -> SwitchTimeouts {
    SwitchTimeouts::uniform(bound(ms))
}

/// Faces that record every call and answer from a script: the listed names
/// reject with `failure` or hang, the rest succeed.
#[derive(Clone)]
struct Scripted {
    calls: Arc<Mutex<Vec<&'static str>>>,
    fail: Vec<&'static str>,
    hang: Vec<&'static str>,
    failure: StepFailure,
}

impl Scripted {
    fn new() -> Self {
        Self {
            calls: Arc::default(),
            fail: Vec::new(),
            hang: Vec::new(),
            failure: StepFailure::step("probe: face rejected"),
        }
    }
    fn failing(mut self, face: &'static str) -> Self {
        self.fail.push(face);
        self
    }
    fn hanging(mut self, face: &'static str) -> Self {
        self.hang.push(face);
        self
    }
    fn with_failure(mut self, failure: StepFailure) -> Self {
        self.failure = failure;
        self
    }
    fn calls(&self) -> Vec<&'static str> {
        self.calls.lock().unwrap().clone()
    }
    fn face(&self, name: &'static str) -> crate::switch::FaceFuture {
        self.calls.lock().unwrap().push(name);
        if self.fail.contains(&name) {
            let failure = self.failure.clone();
            return Box::pin(std::future::ready(Err(failure)));
        }
        if self.hang.contains(&name) {
            return Box::pin(std::future::pending());
        }
        Box::pin(std::future::ready(Ok(())))
    }
}

impl SwitchFaces for Scripted {
    fn stop_automation(&self) -> crate::switch::FaceFuture {
        self.face("stop_automation")
    }
    fn stop_accepting(&self) -> crate::switch::FaceFuture {
        self.face("stop_accepting")
    }
    fn flush(&self) -> crate::switch::FaceFuture {
        self.face("flush")
    }
    fn stop_owner(&self) -> crate::switch::FaceFuture {
        self.face("stop_owner")
    }
    fn recover(&self) -> crate::switch::FaceFuture {
        self.face("recover")
    }
}

/// #1353 A2: the drain-reject path. The matrix's third §三 r3 branch — the
/// stop chain itself rejecting — is the same failure face (probe
/// P-A3-STOPREJ scenario B).
#[tokio::test]
async fn drain_rejection_halts_the_switch() {
    let faces = Scripted::new().failing("stop_automation");
    let machine = SwitchMachine::new(faces.clone(), uniform(2_000));
    let phase = machine.phase();
    let result = machine.run().await;
    // The negative-control anchor (`switch-halt-continues` deletes the halt
    // branch): the receipt-emitting assertion must be the first one the
    // mutant can reach, so the halt check leads.
    negative_assert!(
        matches!(result, Err(SwitchError::Drain(StepFailure::Step(message)))
            if message == "probe: face rejected"),
        "#1353 A2: a rejecting face halts the switch with its own failure"
    );
    negative_assert_eq!(
        faces.calls(),
        vec!["stop_automation"],
        "#1353 A2: no later face runs after a rejection"
    );
    assert_eq!(*phase.borrow(), Phase::Halted);
}

/// #1353 A2 aggregate form: two stop-chain failures surface as one
/// aggregate, the `stopGeneration` shape (probe scenario C).
#[tokio::test]
async fn two_rejections_aggregate_like_stop_generation() {
    let failure = StepFailure::aggregate(
        "Project automation provider cleanup failed.",
        vec![
            "probe: cron stop rejected".into(),
            "probe: alwaysOn stop rejected".into(),
        ],
    );
    let faces = Scripted::new()
        .failing("stop_automation")
        .with_failure(failure);
    let result = SwitchMachine::new(faces.clone(), uniform(2_000))
        .run()
        .await;
    match result {
        Err(SwitchError::Drain(StepFailure::Aggregate { message, errors })) => {
            assert_eq!(message, "Project automation provider cleanup failed.");
            assert_eq!(errors.len(), 2, "#1353 both causes stay in the aggregate");
        }
        other => panic!("#1353 aggregate failure must stay aggregated, got {other:?}"),
    }
    assert_eq!(
        faces.calls(),
        vec!["stop_automation"],
        "#1353 A2: the aggregate halt also stops the switch"
    );
    let rendered = SwitchError::Drain(StepFailure::aggregate(
        "Project automation provider cleanup failed.",
        vec!["one".into(), "two".into()],
    ))
    .to_string();
    assert_eq!(
        rendered,
        "switch halted: AggregateError: Project automation provider cleanup failed. (one; two)",
        "#1353 the aggregate keeps every message in the rendered error"
    );
}

/// #1353 A2 + A10: an entry-face rejection never reaches flush or the owner
/// stop — the flush precondition is the drain itself.
#[tokio::test]
async fn entry_rejection_never_flushes() {
    let faces = Scripted::new().failing("stop_accepting");
    let result = SwitchMachine::new(faces.clone(), uniform(2_000))
        .run()
        .await;
    assert!(matches!(result, Err(SwitchError::Drain(_))));
    assert_eq!(
        faces.calls(),
        vec!["stop_automation", "stop_accepting"],
        "#1353 A10: flush stays unreachable while entries failed to drain"
    );
}

/// #1353 A3: a hung face times out and halts — no "give up waiting and
/// continue" branch exists.
#[tokio::test]
async fn hung_automation_face_times_out() {
    let faces = Scripted::new().hanging("stop_automation");
    let result = SwitchMachine::new(faces.clone(), uniform(100)).run().await;
    match result {
        Err(SwitchError::Timeout { phase, bound }) => {
            assert_eq!(phase, Phase::StopAutomation);
            assert_eq!(bound, Duration::from_millis(100));
        }
        other => panic!("#1353 A3: a hung face must time out, got {other:?}"),
    }
    assert_eq!(
        faces.calls(),
        vec!["stop_automation"],
        "#1353 A3: no later face runs after a timeout"
    );
}

/// #1353 A3 + A10: a hung entry drain blocks flush for the whole bound, and
/// the timeout keeps the machine out of flush.
#[tokio::test]
async fn hung_entry_drain_never_flushes() {
    let faces = Scripted::new().hanging("stop_accepting");
    let result = SwitchMachine::new(faces.clone(), uniform(100)).run().await;
    assert!(matches!(result, Err(SwitchError::Timeout { .. })));
    assert_eq!(
        faces.calls(),
        vec!["stop_automation", "stop_accepting"],
        "#1353 A10: a hung drain keeps flush unreachable"
    );
}

/// #1353 the completing order and its terminal phases.
#[tokio::test]
async fn completing_sequence_runs_every_face_and_switches() {
    let faces = Scripted::new();
    let machine = SwitchMachine::new(faces.clone(), uniform(2_000));
    let phase = machine.phase();
    let gate = machine.gate();
    machine
        .run()
        .await
        .expect("#1353 scripted faces all succeed");
    assert_eq!(
        faces.calls(),
        vec![
            "stop_automation",
            "stop_accepting",
            "flush",
            "stop_owner",
            "recover"
        ],
        "#1353 the matrix §二 order"
    );
    assert_eq!(*phase.borrow(), Phase::Switched);
    assert!(gate.is_open(), "#1353 a completed switch opened the gate");
}

/// A stop-owner face that signals once it is running, then finishes.
struct SlowOwner {
    entered: Arc<tokio::sync::Notify>,
}

impl SwitchFaces for SlowOwner {
    fn stop_automation(&self) -> crate::switch::FaceFuture {
        Box::pin(std::future::ready(Ok(())))
    }
    fn stop_accepting(&self) -> crate::switch::FaceFuture {
        Box::pin(std::future::ready(Ok(())))
    }
    fn flush(&self) -> crate::switch::FaceFuture {
        Box::pin(std::future::ready(Ok(())))
    }
    fn stop_owner(&self) -> crate::switch::FaceFuture {
        let entered = self.entered.clone();
        Box::pin(async move {
            entered.notify_one();
            tokio::time::sleep(Duration::from_millis(150)).await;
            Ok(())
        })
    }
    fn recover(&self) -> crate::switch::FaceFuture {
        Box::pin(std::future::ready(Ok(())))
    }
}

/// #1353 A8: the new-owner gate stays closed while the old owner (its
/// background writers included) is still stopping, and opens only after the
/// whole sequence completed.
#[tokio::test]
async fn gate_stays_closed_until_the_owner_stopped() {
    let entered = Arc::new(tokio::sync::Notify::new());
    let faces = SlowOwner {
        entered: entered.clone(),
    };
    let machine = SwitchMachine::new(faces, uniform(2_000));
    let gate = machine.gate();
    let done = tokio::spawn(machine.run());
    entered.notified().await;
    negative_assert!(
        !gate.is_open(),
        "#1353 A8: the gate is closed while stop_owner is still running"
    );
    done.await
        .expect("#1353 machine task")
        .expect("#1353 scripted switch completes");
    assert!(
        gate.is_open(),
        "#1353 A8: the gate opens after the owner stopped"
    );
}

/// #1353 A9: the new handshake waits on the gate — nothing observes it open
/// before the old owner fully completed.
#[tokio::test]
async fn new_handshake_waits_for_the_gate() {
    let entered = Arc::new(tokio::sync::Notify::new());
    let faces = SlowOwner {
        entered: entered.clone(),
    };
    let machine = SwitchMachine::new(faces, uniform(2_000));
    let gate = machine.gate();
    let observed = Arc::new(AtomicBool::new(false));
    let waiter = {
        let gate = gate.clone();
        let observed = observed.clone();
        tokio::spawn(async move {
            gate.wait_open().await;
            observed.store(true, Ordering::SeqCst);
        })
    };
    let done = tokio::spawn(machine.run());
    entered.notified().await;
    // Yield a full scheduling round: a wait_open that resolved early must
    // have run by now.
    for _ in 0..16 {
        tokio::task::yield_now().await;
    }
    assert!(
        !observed.load(Ordering::SeqCst),
        "#1353 A9: wait_open is still pending while the owner is stopping"
    );
    done.await
        .expect("#1353 machine task")
        .expect("#1353 scripted switch completes");
    tokio::time::timeout(Duration::from_secs(2), waiter)
        .await
        .expect("#1353 wait_open resolves once the gate opened")
        .expect("#1353 waiter task");
    assert!(observed.load(Ordering::SeqCst));
}

/// #1353 A8: a halted switch never opens the gate.
#[tokio::test]
async fn halted_switch_keeps_the_gate_closed() {
    let faces = Scripted::new().failing("stop_accepting");
    let machine = SwitchMachine::new(faces, uniform(2_000));
    let gate = machine.gate();
    let result = machine.run().await;
    assert!(result.is_err());
    assert!(
        !gate.is_open(),
        "#1353 A8: a halted switch keeps the gate closed"
    );
}

/// #1353 DC-1: the shared predicate — positive, finite, capped. The
/// "positive and finite" clause without the cap would pass `u64::MAX`
/// milliseconds and `1e300` (matrix A3 row: an uncapped predicate judges
/// actually-unbounded values as bounded).
#[test]
fn bounded_timeout_predicate_rejects_unbounded_values() {
    for rejected in [
        0.0f64,
        -1.0f64,
        f64::NAN,
        f64::INFINITY,
        f64::NEG_INFINITY,
        u64::MAX as f64,
        1e300f64,
        (TIMEOUT_CAP_MS + 1) as f64,
    ] {
        // The negative-control anchor (`switch-unbounded-timeout` deletes the
        // cap clause): the receipt-emitting form, so the uncapped spellings
        // cannot pass as bounded.
        negative_assert!(
            BoundedTimeout::from_millis_f64(rejected).is_err(),
            "#1353 DC-1: the f64 spelling rejects the uncapped value"
        );
    }
    for accepted in [1.0f64, 5000.0f64, TIMEOUT_CAP_MS as f64] {
        assert!(
            BoundedTimeout::from_millis_f64(accepted).is_ok(),
            "#1353 DC-1: {accepted} must be accepted"
        );
    }
    // The integer spelling routes through the same predicate: the overflowed
    // and zero values are the same rejections.
    for rejected in [0u64, TIMEOUT_CAP_MS + 1, u64::MAX] {
        assert!(
            BoundedTimeout::from_millis(rejected).is_err(),
            "#1353 DC-1: {rejected} must be rejected"
        );
    }
    for accepted in [1u64, 5000, TIMEOUT_CAP_MS] {
        assert!(
            BoundedTimeout::from_millis(accepted).is_ok(),
            "#1353 DC-1: {accepted} must be accepted"
        );
    }
    // first: the overflowed float spelling is the same rejection.
    assert!(BoundedTimeout::from_millis(u64::MAX).is_err());
}

/// #2363: the backstop spelling — the same predicate shape one ceiling
/// higher, exercised on its own instead of through the shared spelling's
/// tests. The backstop's cap clause is a copy in [`BoundedTimeout`], not a
/// call through [`BoundedTimeout::from_millis_f64`], so the shared
/// spelling's coverage proved nothing about this one (the
/// `switch-unbounded-timeout` mutant survived against every backstop test
/// — fake coverage through the cap copy).
#[test]
fn backstop_timeout_predicate_rejects_unbounded_values() {
    for rejected in [
        0.0f64,
        -1.0f64,
        f64::NAN,
        f64::INFINITY,
        f64::NEG_INFINITY,
        u64::MAX as f64,
        1e300f64,
        (TIMEOUT_CAP_MS + BACKSTOP_SLACK_MS + 1) as f64,
    ] {
        // The negative-control anchor
        // (`switch-backstop-unbounded-timeout` deletes the backstop's own
        // cap clause): the receipt-emitting form, so the uncapped spellings
        // cannot pass as bounded backstops.
        negative_assert!(
            BoundedTimeout::backstop_from_millis(rejected).is_err(),
            "#2363 the backstop spelling rejects the uncapped value"
        );
    }
    // The boundary value itself is bounded: the ceiling admits its own
    // edge, and the derived backstops (budget + slack) sit on it.
    for accepted in [
        1.0f64,
        5000.0f64,
        (TIMEOUT_CAP_MS + BACKSTOP_SLACK_MS) as f64,
    ] {
        assert!(
            BoundedTimeout::backstop_from_millis(accepted).is_ok(),
            "#2363 the backstop spelling accepts the capped value {accepted}"
        );
    }
    // Fractional milliseconds round to a nonzero duration — accepting the
    // value must not collapse it to zero.
    assert_eq!(
        BoundedTimeout::backstop_from_millis(0.5)
            .expect("#2363 the fractional backstop is bounded")
            .duration(),
        Duration::from_micros(500),
        "#2363 the backstop spelling keeps the fractional millisecond"
    );
}

/// #1353 DC-1: the operator-facing production bounds stay at the shared
/// cap, and the slack makes the machine bound a backstop rather than a
/// racing copy of the face's own deadline — the backstops are derived
/// (budget capped, then slack added), so at the maximum `--drain-ms` they
/// still sit strictly above the face's own internal deadline.
#[test]
fn production_timeouts_keep_the_shared_cap() {
    let timeouts = SwitchTimeouts::production(Duration::from_millis(TIMEOUT_CAP_MS));
    for bound in [timeouts.automation, timeouts.flush, timeouts.handshake] {
        assert_eq!(
            bound.duration(),
            Duration::from_millis(TIMEOUT_CAP_MS),
            "#1353 the operator-facing bounds stay at the shared cap"
        );
    }
    for bound in [timeouts.entries, timeouts.owner] {
        assert_eq!(
            bound.duration(),
            Duration::from_millis(TIMEOUT_CAP_MS + BACKSTOP_SLACK_MS),
            "#1353 the backstop keeps its slack at the cap"
        );
    }
    let timeouts = SwitchTimeouts::production(Duration::from_millis(5_000));
    assert_eq!(
        timeouts.entries.duration(),
        Duration::from_millis(15_000),
        "#1353 the entries backstop exceeds the internal drain budget"
    );
    assert_eq!(
        timeouts.automation.duration(),
        Duration::from_millis(5_000),
        "#1353 the automation bound matches the drain budget"
    );
}

// ------------------------------------------------------------ the cron face

/// A gateway whose abort always rejects — the failure `CronManager::stop`
/// swallows (A7).
struct RejectingGateway;

impl CronAgentGateway for RejectingGateway {
    fn submit_turn(&self, _input: &crate::runtime::cron::CronTurnSubmit) -> Result<(), CronError> {
        Ok(())
    }
    fn abort_turn(
        &self,
        _session_key: &JsString,
        _run_id: Option<&JsString>,
        _reason: Option<&JsString>,
    ) -> Result<(), CronError> {
        Err(CronError::error("probe: abortTurn rejected"))
    }
    fn close_session(
        &self,
        _session_key: &JsString,
        _reason: Option<&JsString>,
    ) -> Result<(), CronError> {
        Ok(())
    }
}

fn cron_manager(scratch: &Scratch) -> Arc<CronManager> {
    let home = scratch.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    CronManager::new(CronManagerOptions {
        config: CronManagerConfig::default(),
        pilot_home: home.to_string_lossy().into_owned(),
        session_overrides: None,
        now: None,
        uuid: None,
        logger: None,
        scheduler_idle_poll_ms: None,
    })
    .expect("#1353 test cron manager constructs")
}

/// Seeds one project's cron store so a runtime can load for it.
fn seed_cron_project(scratch: &Scratch) -> crate::runtime::cron::CronPaths {
    std::fs::create_dir_all(scratch.path().join("home")).unwrap();
    let paths = crate::runtime::cron::resolve_cron_paths(
        &scratch.path().join("home").to_string_lossy(),
        "/proj",
    )
    .expect("#1353 test cron paths resolve");
    std::fs::create_dir_all(&paths.project_dir).unwrap();
    std::fs::write(&paths.tasks_file, "{\"schemaVersion\":1,\"tasks\":[]}").unwrap();
    paths
}

/// Loads the seeded project's runtime through `createTask`'s `ensureRuntime`
/// (the manager-test precedent: a create on an unstarted manager loads the
/// runtime without starting its scheduler).
fn load_cron_runtime(manager: &CronManager) {
    let input = pilotdeck_protocol::parse_json(
        r#"{
            "projectKey": "/proj",
            "message": "m1",
            "sessionKey": "origin-session",
            "channelKey": "origin-channel",
            "mode": "default",
            "schedule": {"type": "once", "runAt": "2027-01-01T00:00:00.000Z"}
        }"#,
    )
    .expect("#1353 test task input parses");
    manager
        .create_task(&input)
        .expect("#1353 test task loads the runtime");
}

/// #1353 A6/A7: the S1 cron face closes the management face, stops the
/// manager exactly once, and keeps the abort rejections its stop swallowed
/// observable.
#[tokio::test]
async fn cron_stop_face_gates_management_and_reports_abort_failures() {
    let scratch = Scratch::new("switch-cron-s1");
    seed_cron_project(&scratch);
    let manager = cron_manager(&scratch);
    let recording = crate::switch::RecordingCronGateway::wrap(Arc::new(RejectingGateway));
    let failures = recording.failures();
    manager
        .bind_agent_gateway(Arc::new(recording))
        .expect("#1353 test gateway binds");
    load_cron_runtime(&manager);
    let runtime = manager
        .loaded_runtime("/proj")
        .expect("#1353 the create loaded the runtime");
    runtime.register_active_run(CronActiveRun {
        run_id: JsString::from("run-1"),
        task_id: JsString::from("task-1"),
        session_key: JsString::from("cron:task-1"),
        schedule_type: JsString::from("once"),
        stop_requested: false,
    });
    let handle = Arc::new(crate::switch::CronStopHandle::new(
        manager.clone(),
        failures,
    ));
    assert!(
        !crate::switch::cron_management_closed(Some(&manager)),
        "#1353 the management face is open before the switch"
    );
    handle
        .stop_face()
        .await
        .expect("#1353 A7: swallowed abort rejections are not fatal, like the baseline");
    assert!(
        crate::switch::cron_management_closed(Some(&manager)),
        "#1353 A6: the S1 face closed the cron management gate"
    );
    assert!(handle.is_stopped(), "#1353 the manager stopped");
    let recorded = handle.abort_failures();
    assert_eq!(
        recorded.len(),
        1,
        "#1353 A7: the swallowed rejection is recorded"
    );
    assert!(
        recorded[0].contains("run-1") && recorded[0].contains("probe: abortTurn rejected"),
        "#1353 A7: the record names the run and the rejection, got {}",
        recorded[0]
    );
    // The teardown's second stop observes the first instead of repeating it:
    // a second abort round would double the record.
    handle
        .stop_once()
        .expect("#1353 the idempotent second stop observes the first");
    assert_eq!(
        handle.abort_failures().len(),
        1,
        "#1353 the second stop did not abort again"
    );
}

/// #1353 A2: a cron stop rejection is a drain failure — the S1 face surfaces
/// the manager's error instead of swallowing it. The rejection is real: the
/// runtime's `stop` re-reads the task store (manager.rs `stop` →
/// `list_tasks`), so an unreadable file fails the chain.
#[tokio::test]
async fn cron_stop_face_surfaces_manager_rejection() {
    let scratch = Scratch::new("switch-cron-s2");
    let paths = seed_cron_project(&scratch);
    let manager = cron_manager(&scratch);
    load_cron_runtime(&manager);
    // The store's read is tolerant of malformed JSON (the TS parse is), so
    // the real stop-path rejection is an I/O failure: the tasks file itself
    // becomes unreadable (`EISDIR`).
    std::fs::remove_file(&paths.tasks_file).unwrap();
    std::fs::create_dir(&paths.tasks_file).unwrap();
    assert!(
        !crate::switch::cron_management_closed(Some(&manager)),
        "#1353 the management face is open before the switch"
    );
    let handle = Arc::new(crate::switch::CronStopHandle::new(
        manager.clone(),
        crate::switch::AbortRecord::default(),
    ));
    match handle.stop_face().await {
        Err(StepFailure::Step(message)) => assert!(
            message.starts_with("cron stop failed:"),
            "#1353 A2: the manager's own error surfaces, got {message}"
        ),
        other => panic!("#1353 A2: the store failure must reject the face, got {other:?}"),
    }
    // The idempotence flag is set even by a failed stop: the machine already
    // reported it, the teardown must not silently retry and "succeed".
    handle
        .stop_once()
        .expect("#1353 the failed stop is still observed as done");
    // #2494 preserves the legacy mixed-handle/direct-call contract. Native
    // assembly uses one shared native handle and retains failure instead;
    // the native behavior has its own test below.
    manager
        .stop()
        .expect("#2494 the direct stop observes the failed face stop");
}

/// #2494: legacy handle and direct stop share the manager latch. Native
/// assembly does not use this direct-call route: both of its faces use the
/// one native handle so a failed drain remains an error.
#[tokio::test]
async fn a_face_stop_then_the_teardowns_direct_stop_observes_it() {
    let scratch = Scratch::new("switch-cron-s1-direct");
    seed_cron_project(&scratch);
    let manager = cron_manager(&scratch);
    let recording = crate::switch::RecordingCronGateway::wrap(Arc::new(RejectingGateway));
    let failures = recording.failures();
    manager
        .bind_agent_gateway(Arc::new(recording))
        .expect("#2494 test gateway binds");
    load_cron_runtime(&manager);
    let runtime = manager
        .loaded_runtime("/proj")
        .expect("#2494 the create loaded the runtime");
    runtime.register_active_run(CronActiveRun {
        run_id: JsString::from("run-1"),
        task_id: JsString::from("task-1"),
        session_key: JsString::from("cron:task-1"),
        schedule_type: JsString::from("once"),
        stop_requested: false,
    });
    let handle = Arc::new(crate::switch::CronStopHandle::new(
        manager.clone(),
        failures,
    ));
    handle
        .stop_face()
        .await
        .expect("#2494 the S1 face stops the manager once");
    assert_eq!(
        handle.abort_failures().len(),
        1,
        "#2494 the face's stop aborted the in-flight run once"
    );
    // A legacy direct caller has no handle flag; the manager owns its latch.
    manager
        .stop()
        .expect("#2494 the direct stop observes the face's earlier stop");
    assert_eq!(
        handle.abort_failures().len(),
        1,
        "#2494 no second abort round through the direct stop"
    );
    assert_eq!(
        manager.runtime_count(),
        0,
        "#2494 the face's stop stays the effective one"
    );
}

/// A gateway whose first `abort_turn` parks until the test releases it and
/// whose later calls answer `Ok` — the in-body park the #2363 item-6 race
/// test holds a real dispatch in. Only the first call parks: the parked
/// caller is the dispatch, and any later caller is a stop round whose abort
/// must not block a face that runs it synchronously (`CronManager::stop`'s
/// own abort round, or — under the item-6 mutants — the face's stop before
/// the drain has waited for the parked dispatch).
struct ParkingGateway {
    /// `(arrived, released)`: `arrived` flips inside the first `abort_turn`,
    /// `released` ends its park.
    state: Mutex<(bool, bool)>,
    signal: std::sync::Condvar,
}

impl ParkingGateway {
    fn new() -> Self {
        Self {
            state: Mutex::new((false, false)),
            signal: std::sync::Condvar::new(),
        }
    }

    fn arrived(&self) -> bool {
        self.state.lock().expect("#2363 park lock").0
    }

    /// Bounded readiness wait: the dispatch must reach the park, and a test
    /// bug that never sends it must fail, not hang.
    async fn wait_arrived(&self) {
        let deadline = std::time::Instant::now() + Duration::from_secs(30);
        while !self.arrived() {
            assert!(
                std::time::Instant::now() < deadline,
                "#2363 the dispatch never reached its park"
            );
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }

    fn release(&self) {
        let mut state = self.state.lock().expect("#2363 park lock");
        state.1 = true;
        drop(state);
        self.signal.notify_all();
    }

    /// `release()` on any exit path (the storage suite's `BarrierRelease`
    /// shape): a guard held across the whole test body frees the parked
    /// dispatch on every panic that can fire while it is parked — an
    /// anchor, a plain assert, or `wait_arrived`'s own readiness bound —
    /// so a failure stays a red light instead of hanging the test binary
    /// on the `#[tokio::test]` runtime's blocking-pool shutdown. The tests'
    /// explicit `release()` calls stay where they are: the park must end at
    /// the points the test then awaits, which a drop-at-scope-end guard
    /// cannot express.
    fn release_on_drop(&self) -> ParkRelease<'_> {
        ParkRelease(self)
    }
}

/// The drop half of [`ParkingGateway::release_on_drop`]: releases the
/// park when the test body unwinds for any reason. `release()` is
/// idempotent, so a scope that already released explicitly re-releases to
/// a no-op.
struct ParkRelease<'a>(&'a ParkingGateway);

impl Drop for ParkRelease<'_> {
    fn drop(&mut self) {
        self.0.release();
    }
}

impl CronAgentGateway for ParkingGateway {
    fn submit_turn(&self, _input: &crate::runtime::cron::CronTurnSubmit) -> Result<(), CronError> {
        Ok(())
    }
    fn abort_turn(
        &self,
        _session_key: &JsString,
        _run_id: Option<&JsString>,
        _reason: Option<&JsString>,
    ) -> Result<(), CronError> {
        let mut state = self.state.lock().expect("#2363 park lock");
        let first = !state.0;
        state.0 = true;
        self.signal.notify_all();
        if first {
            while !state.1 {
                state = self
                    .signal
                    .wait(state)
                    .expect("#2363 park lock survived a wait");
            }
        }
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

/// #2363 item 6: the S1 face drains a management dispatch already past the
/// A6 gate. The pre-item-6 seam checked the closed registry before the
/// `spawn_blocking`, so a dispatch that had passed the check could run its
/// whole body on the stopped manager after S1 had already reported Ok. The
/// real dispatch path (`dispatch_under_management_gate`, the WS seam's own
/// closure body) runs `cron_stop` against a registered active run whose
/// `abort_turn` parks: the dispatch holds the management gate's read half
/// across its body, so the stop face cannot report Ok until the parked body
/// finishes — and the stop itself lands only after it.
#[tokio::test]
async fn the_stop_face_drains_an_in_flight_management_dispatch() {
    let scratch = Scratch::new("switch-cron-item6");
    seed_cron_project(&scratch);
    let manager = cron_manager(&scratch);
    let parking = Arc::new(ParkingGateway::new());
    let _park_release = parking.release_on_drop();
    manager
        .bind_agent_gateway(parking.clone())
        .expect("#2363 test gateway binds");
    load_cron_runtime(&manager);
    manager
        .loaded_runtime("/proj")
        .expect("#2363 the create loaded the runtime")
        .register_active_run(CronActiveRun {
            run_id: JsString::from("run-1"),
            task_id: JsString::from("task-1"),
            session_key: JsString::from("cron:task-1"),
            schedule_type: JsString::from("once"),
            stop_requested: false,
        });
    let handle = Arc::new(crate::switch::CronStopHandle::new(
        manager.clone(),
        crate::switch::AbortRecord::default(),
    ));
    // The in-flight dispatch: the real routing-seam body, on the blocking
    // pool the seam itself uses, past the (open) gate and parked inside the
    // method body.
    let dispatch_cron = Some(manager.clone());
    let dispatch = tokio::task::spawn_blocking(move || {
        let input = pilotdeck_protocol::parse_json(
            r#"{"projectKey":"/proj","taskId":"task-1","runId":"run-1"}"#,
        )
        .expect("#2363 stopTask input parses");
        crate::native_process::dispatch_under_management_gate(&dispatch_cron, "cron_stop", &input)
    });
    parking.wait_arrived().await;
    // S1 while the dispatch is parked: the mark lands on the face's first
    // poll (inside the timeout below), but the face cannot pass its drain
    // and report Ok yet.
    let mut face = handle.stop_face();
    let elapsed = tokio::time::timeout(Duration::from_millis(500), &mut face).await;
    negative_assert!(
        elapsed.is_err(),
        "#2363 the stop face waits for the in-flight dispatch"
    );
    negative_assert!(
        crate::switch::cron_management_closed(Some(&manager)),
        "#2363 the A6 mark landed on the face's first poll"
    );
    // The parked body finishes; only then the drain acquires, the manager
    // stops and the face reports Ok.
    parking.release();
    let stopped = dispatch
        .await
        .expect("#2363 the parked dispatch joins")
        .expect("#2363 the dispatch is one of the six methods")
        .expect("#2363 the in-flight dispatch completes with its own result");
    negative_assert_eq!(
        stopped.get("stopped").cloned(),
        Some(pilotdeck_protocol::JsValue::Bool(true)),
        "#2363 the drained dispatch answers its method's own result"
    );
    face.await
        .expect("#2363 the S1 face reports Ok only after the drain");
    assert!(
        handle.is_stopped(),
        "#2363 the manager stopped after the drain"
    );
}

/// #2363 item 6, the cancelled cell: a stop face dropped mid-drain (the
/// machine's A3 bound) leaves the closed mark standing — it landed on the
/// first poll, exactly where the pre-item-6 face marked it — while the
/// admitted dispatch finishes on its own and the manager keeps whatever
/// the completed faces left it: nothing stopped, nothing half-stopped.
#[tokio::test]
async fn a_cancelled_stop_face_leaves_the_gate_closed() {
    let scratch = Scratch::new("switch-cron-item6-cancel");
    seed_cron_project(&scratch);
    let manager = cron_manager(&scratch);
    let parking = Arc::new(ParkingGateway::new());
    let _park_release = parking.release_on_drop();
    manager
        .bind_agent_gateway(parking.clone())
        .expect("#2363 test gateway binds");
    load_cron_runtime(&manager);
    manager
        .loaded_runtime("/proj")
        .expect("#2363 the create loaded the runtime")
        .register_active_run(CronActiveRun {
            run_id: JsString::from("run-1"),
            task_id: JsString::from("task-1"),
            session_key: JsString::from("cron:task-1"),
            schedule_type: JsString::from("once"),
            stop_requested: false,
        });
    let handle = Arc::new(crate::switch::CronStopHandle::new(
        manager.clone(),
        crate::switch::AbortRecord::default(),
    ));
    let dispatch_cron = Some(manager.clone());
    let dispatch = tokio::task::spawn_blocking(move || {
        let input = pilotdeck_protocol::parse_json(
            r#"{"projectKey":"/proj","taskId":"task-1","runId":"run-1"}"#,
        )
        .expect("#2363 stopTask input parses");
        crate::native_process::dispatch_under_management_gate(&dispatch_cron, "cron_stop", &input)
    });
    parking.wait_arrived().await;
    // The face is polled inside the window (the mark lands, the drain queues
    // behind the dispatch's read half) and then dropped — the machine's
    // timeout shape. Any assert below that fails while the dispatch is
    // parked releases it through `_park_release`'s drop on unwind.
    {
        let mut face = handle.stop_face();
        let elapsed = tokio::time::timeout(Duration::from_millis(50), &mut face).await;
        negative_assert!(
            elapsed.is_err(),
            "#2363 the cancelled face is parked in its drain, not done"
        );
    }
    assert!(
        crate::switch::cron_management_closed(Some(&manager)),
        "#2363 the cancelled face leaves the closed mark standing"
    );
    assert!(
        !handle.is_stopped(),
        "#2363 the cancelled face stopped nothing"
    );
    parking.release();
    let stopped = dispatch
        .await
        .expect("#2363 the parked dispatch joins")
        .expect("#2363 the dispatch is one of the six methods")
        .expect("#2363 the admitted dispatch finishes on its own");
    negative_assert_eq!(
        stopped.get("stopped").cloned(),
        Some(pilotdeck_protocol::JsValue::Bool(true)),
        "#2363 the un-stopped manager served the admitted dispatch"
    );
    // The mark the cancelled face left is not a stray: a fresh face still
    // drains nothing (the dispatch finished) and stops exactly once.
    handle
        .stop_face()
        .await
        .expect("#2363 a fresh face completes the stop");
    assert!(
        handle.is_stopped(),
        "#2363 the fresh face stopped the manager"
    );
}

/// #2363 (codex concurrency P1 fix): the gate registry keys managers
/// weakly and sweeps dead entries on lookup — it must never keep a dropped
/// manager (and with it the manager's scheduler cleanup, which only its
/// `CronRuntime::drop` wakes) alive past the host's own references. A
/// strong key here would retire that cleanup to process exit for every
/// ordinary host that served one management dispatch. The assertions are
/// identity-based (weak handles to this test's own manager and gate), not
/// registry-wide counts: the registry is process-global, so a census would
/// be perturbed by sibling tests registering their own managers — and the
/// lookups themselves compare keys by pointer without upgrading them, so a
/// sibling's in-flight lookup cannot lend this test's dropped manager a
/// fleeting strong reference and turn the death assertion into a
/// parallel-run false failure.
#[test]
fn the_management_gate_registry_forgets_dead_managers() {
    let scratch = Scratch::new("switch-cron-gate-registry");
    let first = cron_manager(&scratch);
    let gate = crate::switch::cron_management_gate(&first);
    let first_manager = Arc::downgrade(&first);
    let first_gate = Arc::downgrade(&gate);
    drop(gate);
    drop(first);
    // The registry must not be what keeps the manager alive: with this
    // test's strong references gone, the weak key is already dead. (A
    // revert to strong keys fails here — the registry would hold it.)
    assert!(
        first_manager.upgrade().is_none(),
        "#2363 the registry does not keep the dropped manager alive"
    );
    // Any later lookup sweeps the dead entry before its own
    // find-or-create, releasing the gate that entry held. (With the sweep
    // removed, the dead-key entry — and its gate — would survive; with it,
    // the weak gate is dead once the sweep has run.)
    let second = cron_manager(&scratch);
    let second_gate = crate::switch::cron_management_gate(&second);
    assert!(
        first_gate.upgrade().is_none(),
        "#2363 the dropped manager's entry is swept, not retained"
    );
    // The lookup half of the contract: a repeat lookup of a live manager
    // must reuse the registered gate. (With the registration — or the
    // pointer-identity find — gone, every lookup would mint a fresh gate
    // and this fails.)
    let again = crate::switch::cron_management_gate(&second);
    negative_assert!(
        Arc::ptr_eq(&again, &second_gate),
        "#2363 the lookup reuses the registered gate"
    );
}

#[test]
fn closed_cron_gate_does_not_retain_manager_or_close_another_manager() {
    let scratch = Scratch::new("switch-cron-weak-close");
    let manager = cron_manager(&scratch);
    let other = cron_manager(&scratch);
    let weak = Arc::downgrade(&manager);
    assert!(!crate::switch::cron_management_closed(Some(&manager)));
    crate::switch::close_cron_management(&manager);
    crate::switch::close_cron_management(&manager);
    assert!(crate::switch::cron_management_closed(Some(&manager)));
    assert!(!crate::switch::cron_management_closed(Some(&other)));
    drop(manager);
    assert!(
        weak.upgrade().is_none(),
        "closed gate must not keep the gateway alive"
    );
    assert!(!crate::switch::cron_management_closed(Some(&other)));
}

/// The shipped native assembly gives the S1 face and provider teardown the
/// same handle. A failed drain stays failed and invokes cancellation once.
#[tokio::test]
async fn native_cron_stop_retains_failure_across_face_and_provider_teardown() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let scratch = Scratch::new("native-cron-stop-repeat");
    let paths = seed_cron_project(&scratch);
    let manager = cron_manager(&scratch);
    load_cron_runtime(&manager);
    std::fs::remove_file(&paths.tasks_file).unwrap();
    std::fs::create_dir(&paths.tasks_file).unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let stopping = calls.clone();
    let handle = Arc::new(crate::switch::CronStopHandle::new_native(
        manager,
        crate::switch::AbortRecord::default(),
        Duration::from_secs(1),
        Arc::new(move || {
            stopping.fetch_add(1, Ordering::SeqCst);
        }),
    ));
    assert!(handle.stop_face().await.is_err());
    let first = handle
        .stop_once()
        .expect_err("failed native drain remains failed");
    let second = handle
        .stop_async()
        .await
        .expect_err("provider teardown sees the same failure");
    assert_eq!(first.to_string(), second.to_string());
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "native cancellation runs once"
    );
}
