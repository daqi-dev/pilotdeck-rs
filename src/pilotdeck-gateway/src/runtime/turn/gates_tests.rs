use super::*;
use crate::{
    runtime::project::{ProjectRuntimeRegistry, ResourceBundle},
    storage::{JsonlTranscriptStore, TranscriptKey},
};

// Last-owner destruction is a deterministic boundary for the dispose count.
struct GateHandler {
    inner: Arc<Handler>,
    released: Arc<AtomicBool>,
}
impl HostHandler for GateHandler {
    fn module_call(&self, call: JsValue) -> HostFuture<JsObject> {
        self.inner.module_call(call)
    }
    fn event(&self, event: JsValue) -> HostFuture<()> {
        self.inner.event(event)
    }
    fn validate_event(&self, payload: &JsValue) -> Result<EventGuard, HostError> {
        self.inner.validate_event(payload)
    }
    fn drain_events(&self) -> Result<Vec<JsObject>, HostError> {
        self.inner.drain_events()
    }
    fn dispose(&self) -> HostFuture<()> {
        self.inner.dispose()
    }
}
impl Drop for GateHandler {
    fn drop(&mut self) {
        self.released.store(true, Ordering::SeqCst);
    }
}

async fn matrix(name: &str) {
    let oracle = crate::test_baseline::oracle(include_str!(
        "../../../tests/fixtures/turn-gates/oracle.json"
    ));
    let JsValue::Array(rows) = oracle.get("matrix").expect("#743 gates prerequisite") else {
        panic!("#743 gates matrix")
    };
    let row = rows
        .iter()
        .find(|r| r.get("name") == Some(&name.into()))
        .expect("#743 gates prerequisite");
    let counts = row.get("counts").expect("#743 gates prerequisite");
    let expected = |key| {
        counts
            .get(key)
            .expect("#743 gates prerequisite")
            .as_number()
            .expect("#743 gates prerequisite") as usize
    };
    let root = std::env::temp_dir().join(format!("pdrs-turn-gates-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&root).expect("#743 gates prerequisite");
    let registry = Arc::new(ProjectRuntimeRegistry::new(
        &root,
        Arc::new(|_: &std::path::Path, _: &mut ResourceBundle| Ok(())),
    ));
    let runtime = registry.resolve(None).expect("#743 gates prerequisite");
    let key = TranscriptKey {
        project_root: root.clone(),
        pilot_home: root.join("home"),
        session_id: "session".into(),
        transcript_path: root.join("session.jsonl"),
    };
    let (entered, mut entry) = oneshot::channel();
    let (release, wait) = std::sync::mpsc::channel();
    let store = Arc::new(RecordingStore {
        disk: JsonlTranscriptStore::new(),
        kind: if name == "start-failure" {
            "turn_started"
        } else if name == "accept-failure" {
            "accepted_input"
        } else {
            "turn_result"
        },
        fail: if name.ends_with("always") {
            usize::MAX
        } else {
            usize::from(name.ends_with("failure") || name.ends_with("once"))
        },
        completes: AtomicUsize::new(0),
        attempts: AtomicUsize::new(0),
        gate: Mutex::new(Some((entered, wait))),
    });
    let recorder = Arc::new(Recorder::new(store.clone(), key.clone()));
    let calls = Arc::new(AtomicUsize::new(0));
    let children = Arc::new(Mutex::new(Vec::new()));
    let connecting = Arc::new(AtomicUsize::new(0));
    let handler = Arc::new(Handler::default());
    let released = Arc::new(AtomicBool::new(false));
    let marker = root.join("execute");
    let session = LiveSessionAdapter::new(recorder, Vec::new, {
        let owner = registry.clone();
        let calls = calls.clone();
        let connecting = connecting.clone();
        let children = children.clone();
        let handler = handler.clone();
        let released = released.clone();
        let marker = marker.clone();
        let mode = name.to_owned();
        move |_| {
            calls.fetch_add(1, Ordering::SeqCst);
            let lease = Arc::new(owner.acquire(None).expect("#743 gates prerequisite"));
            let children = children.clone();
            let handler = Arc::new(GateHandler {
                inner: handler.clone(),
                released: released.clone(),
            });
            let marker = marker.clone();
            let mode = mode.clone();
            let connecting = connecting.clone();
            Box::pin(async move {
                let connect: HostFuture<HostClient> = Box::pin(async move {
                    connecting.fetch_add(1, Ordering::SeqCst);
                    let mut command = Command::new("python3");
                    command
                        .arg(concat!(
                            env!("CARGO_MANIFEST_DIR"),
                            "/tests/fixtures/turn-gates/peer.py"
                        ))
                        .arg(mode)
                        .arg(marker);
                    let client = HostClient::spawn(
                        &mut command,
                        Some(&"fixture".into()),
                        Limits {
                            handshake_timeout: Duration::from_secs(5),
                            execution_timeout: Some(Duration::from_secs(10)),
                            ..Limits::default()
                        },
                    )
                    .await?;
                    children
                        .lock()
                        .expect("#743 gates prerequisite")
                        .push(client.child_id().expect("#743 gates prerequisite"));
                    Ok(client)
                });
                Ok((connect, handler as Arc<dyn HostHandler>, lease))
            })
        }
    });
    let mut turn = Some(
        session
            .submit_turn_for_test(input(), options())
            .expect("#743 gates prerequisite"),
    );
    let mut events = Vec::new();
    let mut error = false;
    let mut gated = false;
    for _ in 0..30 {
        let result = if !gated && name != "drop-before" {
            let mut pending = turn.as_mut().expect("#743 gates prerequisite").next();
            let progress = timeout(Duration::from_secs(15), async {
                tokio::select! { biased; _ = &mut entry => None, value = &mut pending => Some(value) }
            }).await.expect("#743 gates gate or next must settle");
            drop(pending);
            if let Some(value) = progress {
                value
            } else {
                gated = true;
                assert_eq!(
                    events
                        .iter()
                        .filter(|e: &&JsValue| e.get("type") == Some(&"turn_completed".into()))
                        .count(),
                    0,
                    "#743 gates no terminal before complete"
                );
                assert!(
                    session
                        .state
                        .lock()
                        .expect("#743 gates prerequisite")
                        .active
                        .is_some(),
                    "#743 gates active slot"
                );
                assert!(
                    session
                        .submit_turn_for_test(input(), turn_options("replacement"))
                        .is_err(),
                    "#743 gates pending write blocks admission"
                );
                for _ in 0..2 {
                    let mut pending = turn.as_mut().expect("#743 gates prerequisite").next();
                    assert_recording_pending(
                        pending.as_mut(),
                        "#743 gates same pending write after cancelled borrow",
                    );
                }
                if matches!(name, "start-failure" | "accept-failure") {
                    crate::negative_assert_eq!(
                        connecting.load(Ordering::SeqCst),
                        0,
                        "#743 gates failed persistence never connects"
                    );
                }
                let pids = children.lock().expect("#743 gates prerequisite").clone();
                for pid in pids {
                    assert_reaped(pid).await;
                }
                assert_eq!(
                    store.attempts.load(Ordering::SeqCst),
                    1,
                    "#743 gates single pending write"
                );
                if name.starts_with("drop-pending") {
                    drop(turn.take());
                    assert!(
                        session
                            .state
                            .lock()
                            .expect("#743 gates prerequisite")
                            .active
                            .is_some(),
                        "#743 gates worker owns slot after drop"
                    );
                    assert_eq!(
                        runtime.lease_count(),
                        1,
                        "#743 gates worker owns lease after drop"
                    );
                    release.send(()).expect("#743 gates prerequisite");
                    break;
                }
                release.send(()).expect("#743 gates prerequisite");
                next(turn.as_mut().expect("#743 gates prerequisite").as_mut()).await
            }
        } else {
            next(turn.as_mut().expect("#743 gates prerequisite").as_mut()).await
        };
        match result {
            Ok(Some(event)) => {
                crate::negative_assert!(
                    name != "start-failure",
                    "#743 gates start failure emits no event"
                );
                let kind = event.get("type").cloned();
                events.push(event);
                if kind == Some("model_event".into()) {
                    if name.starts_with("cancel-") {
                        session
                            .abort(&"user-stop".into())
                            .expect("#743 gates prerequisite");
                    }
                    if name == "drop-before" {
                        drop(turn.take());
                        break;
                    }
                }
                if kind == Some("turn_completed".into()) && name == "drop-after" {
                    drop(turn.take());
                    break;
                }
            }
            Ok(None) => break,
            Err(_) => {
                error = true;
                assert_eq!(
                    next(turn.as_mut().expect("#743 gates prerequisite").as_mut())
                        .await
                        .expect("#743 gates prerequisite"),
                    None,
                    "#743 gates failed stream closes"
                );
                break;
            }
        }
    }
    if let Some(turn) = turn.as_mut() {
        assert_eq!(
            next(turn.as_mut()).await.expect("#743 gates prerequisite"),
            None,
            "#743 gates terminal is unique"
        );
    }
    drop(turn);
    timeout(Duration::from_secs(15), async {
        while session
            .state
            .lock()
            .expect("#743 gates prerequisite")
            .active
            .is_some()
            || runtime.lease_count() != 0
            || !released.load(Ordering::SeqCst)
        {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("#743 gates owned cleanup must release slot and lease");
    let pids = children.lock().expect("#743 gates prerequisite").clone();
    for pid in &pids {
        assert_reaped(*pid).await;
    }
    assert_eq!(
        calls.load(Ordering::SeqCst),
        expected("factory"),
        "#743 gates factory count"
    );
    assert_eq!(
        std::fs::read_to_string(&marker)
            .unwrap_or_default()
            .lines()
            .count(),
        expected("loop"),
        "#743 gates execute count"
    );
    assert_eq!(
        connecting.load(Ordering::SeqCst),
        expected("connect"),
        "#743 gates connection attempts"
    );
    assert_eq!(
        pids.len(),
        expected("connect"),
        "#743 gates connection count"
    );
    assert_eq!(
        handler.calls.load(Ordering::SeqCst),
        expected("model"),
        "#743 gates model count"
    );
    assert_eq!(
        store.completes.load(Ordering::SeqCst),
        if name.starts_with("drop-pending") {
            1
        } else {
            expected("complete")
        },
        "#743 gates complete count"
    );
    assert_eq!(
        pids.len(),
        expected("closed"),
        "#743 gates all constructed children reaped"
    );
    assert_eq!(
        handler.disposed.load(Ordering::SeqCst),
        1,
        "#743 gates handler cleanup exactly once"
    );
    let terminals: Vec<_> = events
        .iter()
        .filter(|e| e.get("type") == Some(&"turn_completed".into()))
        .collect();
    let expected_terminals = if name.starts_with("drop-pending") {
        0
    } else {
        row.get("terminal")
            .expect("#743 gates prerequisite")
            .as_number()
            .expect("#743 gates prerequisite") as usize
    };
    assert_eq!(
        terminals.len(),
        expected_terminals,
        "#743 gates host terminal count"
    );
    if let Some(terminal) = terminals.first() {
        assert_eq!(
            terminal
                .get("result")
                .expect("#743 gates prerequisite")
                .get("type"),
            row.get("result"),
            "#743 gates result kind"
        );
    }
    assert_eq!(
        error,
        !name.starts_with("drop-pending") && row.get("error") != Some(&JsValue::Null),
        "#743 gates completion failure propagates"
    );
    let bytes = std::fs::read_to_string(&key.transcript_path).unwrap_or_default();
    let durable = bytes
        .lines()
        .filter(|line| {
            parse_json(line)
                .expect("#743 gates prerequisite")
                .get("type")
                == Some(&"turn_result".into())
        })
        .count();
    assert_eq!(
        durable,
        if name == "drop-pending" {
            1
        } else {
            expected_terminals
        },
        "#743 gates durable terminal count"
    );
    let replacement = session
        .submit_turn_for_test(input(), turn_options("replacement"))
        .expect("#743 gates prerequisite");
    drop(replacement);
    registry.dispose().await.expect("#743 gates prerequisite");
    std::fs::remove_dir_all(root).expect("#743 gates prerequisite");
}
macro_rules! gate_cases {
    ($($name:ident, $case:literal;)*) => {
        $(#[tokio::test]
        async fn $name() { matrix($case).await; })*
        #[test]
        fn gates_oracle_fully_consumed() {
            let oracle = crate::test_baseline::oracle(include_str!(
                "../../../tests/fixtures/turn-gates/oracle.json"
            ));
            let JsValue::Array(rows) = oracle.get("matrix").unwrap() else { panic!("matrix") };
            assert_fixture_consumed(rows, [$($case),*].into_iter().map(str::to_owned));
        }
    };
}
gate_cases! {
    gates_start_failure, "start-failure";
    gates_accept_failure, "accept-failure";
    gates_cancel_ok, "cancel-ok";
    gates_cancel_once, "cancel-once";
    gates_cancel_always, "cancel-always";
    gates_error_ok, "error-ok";
    gates_error_once, "error-once";
    gates_error_always, "error-always";
    gates_protocol_ok, "protocol-ok";
    gates_protocol_once, "protocol-once";
    gates_protocol_always, "protocol-always";
    gates_drop_before, "drop-before";
    gates_drop_pending, "drop-pending";
    gates_drop_after, "drop-after";
    gates_drop_pending_once, "drop-pending-once";
    gates_drop_pending_always, "drop-pending-always";
}

#[test]
#[ignore = "requires fixed PDRS_BASELINE and Node/tsx"]
fn gates_regenerates_identically() {
    crate::test_baseline::regenerates_identically("turn-gates");
}

/// #855: the host-event pump starts strictly after both durable gates commit.
/// A gate that rejects or is cancelled must never start a pump at all — not
/// only never drain one — so every scenario also pins the with_pump entry
/// counter (`pump_starts_for_test`; entry arms the 500 ms timer) at its
/// terminal and again after owned cleanup. The open case pins the start order
/// itself: exactly one poll past the open gate must not tick, because correct
/// code only enters with_pump in that poll, and the whole turn enters exactly
/// once. A pump wrapped before the gate arms its timer at admission (the
/// factory poll the gate entry already proves happened), so by gate-open that
/// timer is long due and the same single poll ticks it.
async fn gates_pump_starts_only_after_the_gate_commits(case: &str) {
    let pump_starts = || super::super::pump_starts_for_test();
    super::super::reset_pump_starts_for_test();
    let root = std::env::temp_dir().join(format!("pdrs-pump-gates-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&root).expect("#855: pump gate setup/read/cleanup must succeed");
    let registry = Arc::new(ProjectRuntimeRegistry::new(
        &root,
        Arc::new(|_: &std::path::Path, _: &mut ResourceBundle| Ok(())),
    ));
    let runtime = registry
        .resolve(None)
        .expect("#855: pump gate setup/read/cleanup must succeed");
    let key = TranscriptKey {
        project_root: root.clone(),
        pilot_home: root.join("home"),
        session_id: "session".into(),
        transcript_path: root.join("session.jsonl"),
    };
    let (entered, mut entry) = oneshot::channel();
    let (release, wait) = std::sync::mpsc::channel();
    let store = Arc::new(RecordingStore {
        disk: JsonlTranscriptStore::new(),
        kind: "turn_started",
        fail: usize::from(case == "reject"),
        completes: AtomicUsize::new(0),
        attempts: AtomicUsize::new(0),
        gate: Mutex::new((case != "reject").then_some((entered, wait))),
    });
    let recorder = Arc::new(Recorder::new(store.clone(), key.clone()));
    let handler = Arc::new(Handler::default());
    // Pre-spawned so the factory poll that arms a pre-gate pump happens
    // promptly; the peer only sends its first frame after execute starts.
    let client = spawn_client("live-payload")
        .await
        .expect("#855: pump gate setup/read/cleanup must succeed");
    let pid = client
        .child_id()
        .expect("#855: pump gate setup/read/cleanup must succeed");
    let initial = Mutex::new(Some(client));
    let session = LiveSessionAdapter::new(recorder, Vec::new, {
        let owner = registry.clone();
        let handler = handler.clone();
        move |_| {
            let lease = Arc::new(
                owner
                    .acquire(None)
                    .expect("#855: pump gate setup/read/cleanup must succeed"),
            );
            let client = initial
                .lock()
                .expect("#855: pump gate setup/read/cleanup must succeed")
                .take()
                .expect("#855: pump gate setup/read/cleanup must succeed");
            let handler = handler.clone();
            Box::pin(async move {
                Ok((
                    Box::pin(async move { Ok(client) }) as HostFuture<HostClient>,
                    handler as Arc<dyn HostHandler>,
                    lease,
                ))
            })
        }
    });
    let mut projection = Vec::new();
    let mut turn = session
        .submit_turn_for_test(input(), options())
        .expect("#855: pump gate setup/read/cleanup must succeed");
    if case == "reject" {
        let error = next(turn.as_mut())
            .await
            .expect_err("#855 [reject]: start write must fail");
        assert!(
            error.message.contains("recording write failed"),
            "#855 [reject]: recording error must survive: {error:?}"
        );
        // The failure terminal is this scenario's end state: the stream
        // already released the execution future, so a pump that started
        // anywhere before it is a gate-order violation.
        assert_eq!(
            pump_starts(),
            0,
            "#855 [reject]: the pump must never start, not even before the failure terminal"
        );
    } else {
        {
            let mut waiting = turn.next();
            timeout(Duration::from_secs(15), async {
                    tokio::select! {
                        biased;
                        _ = &mut entry => {},
                        result = &mut waiting => panic!("#855 [{case}]: recording gate must precede delivery: {result:?}"),
                    }
                })
                .await
                .expect("#855: append must enter the gate");
            // The gate holds: with the preflight write parked, next() never
            // reaches the select that polls the execution future, so no
            // pump can have ticked, started or drained anything.
            assert_eq!(
                pump_starts(),
                0,
                "#855 [{case}]: no pump entry while the gate holds"
            );
            assert_eq!(
                handler.drain_calls.load(Ordering::SeqCst),
                0,
                "#855 [{case}]: no pump tick while the gate holds"
            );
            assert_eq!(
                handler.calls.load(Ordering::SeqCst),
                0,
                "#855 [{case}]: no module call before the gate commits"
            );
            assert!(
                handler
                    .events
                    .lock()
                    .expect("#855: pump gate setup/read/cleanup must succeed")
                    .is_empty(),
                "#855 [{case}]: no host event before the gate commits"
            );
            if case == "open" {
                // A pump armed at admission is already due by gate-open.
                tokio::time::sleep(Duration::from_millis(700)).await;
                assert_eq!(
                    handler.drain_calls.load(Ordering::SeqCst),
                    0,
                    "#855 [{case}]: still no pump tick inside the closed window"
                );
            }
            if case == "cancel" {
                session
                    .abort(&"user-stop".into())
                    .expect("#855: pump gate setup/read/cleanup must succeed");
            }
        } // Cancel only next's borrow; the same driver stays owned.
        release
            .send(())
            .expect("#855: pump gate setup/read/cleanup must succeed");
        let started = next(turn.as_mut())
            .await
            .expect("#855: start read must succeed")
            .expect("#855: start event must exist");
        projection.push(started.get("type").expect("#855: start event type").clone());
        assert_eq!(
            started.get("type"),
            Some(&"turn_started".into()),
            "#855 [{case}]: start must be the first committed gate event"
        );
        let accepted = next(turn.as_mut())
            .await
            .expect("#855: accepted input read must succeed")
            .expect("#855: accepted input event must exist");
        projection.push(
            accepted
                .get("type")
                .expect("#855: accepted input event type")
                .clone(),
        );
        assert_eq!(
            accepted.get("type"),
            Some(&"input_accepted".into()),
            "#855 [{case}]: accepted input follows start"
        );
        // Node v22.22.0: this host prefix survives even cancellation at the
        // start gate. Consume exactly one event before polling execution.
        let submitted = next(turn.as_mut())
            .await
            .expect("#741k: submitted prompt read must succeed")
            .expect("#741k: submitted prompt event must exist");
        assert_eq!(
            submitted.get("type"),
            Some(&"user_prompt_submitted".into()),
            "#741k [{case}]: user_prompt_submitted must follow input_accepted"
        );
        projection.push(
            submitted
                .get("type")
                .expect("#741k: submitted prompt event type")
                .clone(),
        );
        if case == "open" {
            // Exactly one poll past the open gate: this poll sends
            // execute_ready and polls the execution future once. Correct
            // code enters with_pump now, so its freshly armed timer cannot
            // be due within this poll; a pump wrapped before the gate
            // drained on its long-expired timer right here.
            let mut pending = turn.next();
            assert_recording_pending(
                pending.as_mut(),
                "#855: execution must not finish at gate open",
            );
            assert_eq!(
                handler.drain_calls.load(Ordering::SeqCst),
                0,
                "#855 [{case}]: the pump must not tick at gate open"
            );
            drop(pending);
        }
        if case == "cancel" {
            assert_aborted(
                &next(turn.as_mut())
                    .await
                    .expect("#855: pump gate setup/read/cleanup must succeed")
                    .expect("#855: pump gate setup/read/cleanup must succeed"),
            );
            // The abort terminal is this scenario's end state: the
            // cancellation must have won the select past the open gate
            // before with_pump was ever entered.
            assert_eq!(
                pump_starts(),
                0,
                "#855 [cancel]: the pump must never start, not even before the abort terminal"
            );
        } else {
            // Node v22.22.0, real TS gates/pump probe: model_request_started
            // survives between durable admission and the authoritative final.
            // Do not skip arbitrary events: the minimal peer has exactly one.
            let model = next(turn.as_mut())
                .await
                .expect("#741i [open]: model request read must succeed")
                .expect("#741i [open]: model request event must exist");
            assert_eq!(
                model.get("type"),
                Some(&"model_request_started".into()),
                "#741i [open]: model_request_started must precede turn_completed"
            );
            projection.push(model.get("type").expect("#741i: model event type").clone());
            let completed = next(turn.as_mut())
                .await
                .expect("#855: pump gate setup/read/cleanup must succeed")
                .expect("#855: pump gate setup/read/cleanup must succeed");
            projection.push(
                completed
                    .get("type")
                    .expect("#741i: completed event type")
                    .clone(),
            );
            assert_eq!(
                completed.get("type"),
                Some(&"turn_completed".into()),
                "#741i [open]: authoritative terminal follows model request"
            );
            assert_eq!(
                completed
                    .get("result")
                    .expect("#855: pump gate setup/read/cleanup must succeed")
                    .get("type"),
                Some(&"success".into()),
                "#741i [open]: authoritative terminal keeps success"
            );
        }
    }
    assert_eq!(
        next(turn.as_mut())
            .await
            .expect("#855: pump gate setup/read/cleanup must succeed"),
        None,
        "#855 [{case}]: stream closes once"
    );
    let oracle = crate::test_baseline::oracle(include_str!(
        "../../../tests/fixtures/pump-order-cases/oracle.json"
    ));
    let Some(JsValue::Array(rows)) = oracle.get("cases") else {
        panic!("#741i: pump oracle cases");
    };
    let row = rows
        .iter()
        .find(|row| row.get("name") == Some(&case.into()))
        .expect("#741i: pump oracle case exists");
    // The pre-execute cancellation terminal is the existing, unapproved
    // A7.2-TURN-ABORT-001 difference; assert_aborted checks it separately.
    assert_eq!(
        Some(&JsValue::Array(projection)),
        row.get("projection"),
        "#741i [{case}]: consumed gate stream matches real TS projection"
    );
    drop(turn);
    timeout(Duration::from_secs(15), async {
        while session
            .state
            .lock()
            .expect("#855: pump gate setup/read/cleanup must succeed")
            .active
            .is_some()
            || runtime.lease_count() != 0
            || handler.disposed.load(Ordering::SeqCst) == 0
        {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("#855: owned cleanup must release slot, lease and handler");
    assert_reaped(pid).await;
    // #855: post-cleanup re-check — neither the terminal nor the owned
    // cleanup may start (or re-arm) a pump after the scenario's end state.
    assert_eq!(
        pump_starts(),
        usize::from(case == "open"),
        "#855 [{case}]: the pump must start only after the gate commits"
    );
    std::fs::remove_dir_all(root).expect("#855: pump gate setup/read/cleanup must succeed");
}

#[test]
#[ignore = "requires fixed PDRS_BASELINE and Node v22.22.0/tsx"]
fn pump_order_regenerates_identically() {
    crate::test_baseline::regenerates_identically("pump-order-cases");
}

/// Each scenario is its own test so a gate-order mutant fails all three
/// independently, with the failing cell identifiable in the log.
#[tokio::test]
async fn gates_pump_reject_never_starts_the_pump() {
    gates_pump_starts_only_after_the_gate_commits("reject").await;
}

#[tokio::test]
async fn gates_pump_cancel_never_starts_the_pump() {
    gates_pump_starts_only_after_the_gate_commits("cancel").await;
}

#[tokio::test]
async fn gates_pump_open_pins_the_start_order() {
    gates_pump_starts_only_after_the_gate_commits("open").await;
}

#[tokio::test]
async fn gates_factory_ready_cancel() {
    struct Owner(Arc<AtomicUsize>, Arc<RuntimeLease>);
    impl Drop for Owner {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }
    impl HostHandler for Owner {
        fn module_call(&self, _: JsValue) -> HostFuture<JsObject> {
            let _ = &self.1;
            Box::pin(async { panic!("unexecuted host") })
        }
    }
    let drops = Arc::new(AtomicUsize::new(0));
    let lease = test_lease();
    let runtime = lease.runtime().clone();
    let client = spawn_client("live-payload")
        .await
        .expect("#743 ready-cancel prerequisite");
    let pid = client.child_id().expect("#743 ready-cancel prerequisite");
    let (send, wait) = oneshot::channel();
    let wait = Mutex::new(Some(wait));
    let prepared = Arc::new(AtomicBool::new(false));
    let consumed = prepared.clone();
    let session = LiveSessionAdapter::new(test_recorder(), Vec::new, move |_| {
        let wait = wait
            .lock()
            .expect("#743 ready-cancel prerequisite")
            .take()
            .expect("#743 ready-cancel prerequisite");
        let consumed = consumed.clone();
        Box::pin(async move {
            let resources = wait.await.expect("#743 ready-cancel prepared resources");
            consumed.store(true, Ordering::SeqCst);
            Ok(resources)
        })
    });
    let mut turn = session
        .submit_turn_for_test(input(), options())
        .expect("#743 ready-cancel prerequisite");
    {
        let mut pending = turn.next();
        assert_recording_pending(pending.as_mut(), "factory waits for prepared resources");
    }
    let handler: Arc<dyn HostHandler> = Arc::new(Owner(drops.clone(), lease.clone()));
    let connect: HostFuture<HostClient> = Box::pin(async move { Ok(client) });
    assert!(
        send.send((connect, handler, lease)).is_ok(),
        "#743 ready-cancel resources sent"
    );
    session
        .abort(&"same tick".into())
        .expect("#743 ready-cancel prerequisite");
    let event = next(turn.as_mut()).await;
    crate::negative_assert!(
        !prepared.load(Ordering::SeqCst),
        "#743 gates pre-cancel skips factory"
    );
    assert_aborted(
        &event
            .expect("#743 ready-cancel next")
            .expect("#743 ready-cancel terminal"),
    );
    assert_eq!(
        next(turn.as_mut())
            .await
            .expect("#743 ready-cancel prerequisite"),
        None,
        "#743 ready-cancel closes"
    );
    assert_reaped(pid).await;
    forwarding::wait_for_cleanup(&session.state, &runtime).await;
    assert_eq!(
        drops.load(Ordering::SeqCst),
        1,
        "prepared handler dropped exactly once"
    );
    assert_eq!(runtime.lease_count(), 0, "prepared lease released");
    assert!(
        session
            .state
            .lock()
            .expect("#743 ready-cancel prerequisite")
            .active
            .is_none(),
        "prepared slot released"
    );
}
