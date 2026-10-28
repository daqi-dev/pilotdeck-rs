use super::*;
use crate::runtime::hooks::LifecycleDispatchResult;
use crate::{host::modules::lifecycle::LifecyclePort, runtime::session::LiveSession};
use serde_json::{Value, json};
use tokio::{
    sync::Semaphore,
    time::{Duration, timeout},
};

fn lock<T>(value: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    value.lock().expect("#743 session matrix mutex")
}
fn value(input: &JsValue) -> Value {
    serde_json::from_str(
        &pilotdeck_protocol::stringify_json(input)
            .expect("#743 session matrix stringify")
            .expect("#743 session matrix defined"),
    )
    .expect("#743 session matrix json")
}
async fn bounded<T>(future: impl std::future::Future<Output = T>) -> T {
    timeout(Duration::from_secs(5), future)
        .await
        .expect("#743 session matrix bounded wait")
}
struct Gate {
    entered: Semaphore,
    release: Semaphore,
}
impl Gate {
    fn new() -> Self {
        Self {
            entered: Semaphore::new(0),
            release: Semaphore::new(0),
        }
    }
    async fn wait(&self) {
        self.entered.add_permits(1);
        self.release
            .acquire()
            .await
            .expect("#743 session matrix release")
            .forget();
    }
    async fn entered(&self) {
        self.entered
            .acquire()
            .await
            .expect("#743 session matrix entered")
            .forget();
    }
    fn open(&self) {
        self.release.add_permits(1);
    }
}
struct Port {
    action: String,
    gate: Arc<Gate>,
    calls: Arc<Mutex<Vec<Value>>>,
}
impl LifecyclePort for Port {
    fn dispatch(&self, input: LifecycleDispatchInput) -> HostFuture<LifecycleDispatchResult> {
        let event = input.event.to_utf8().expect("#743 session matrix event");
        lock(&self.calls).push(json!({"event":event,"baseInput":value(&JsValue::Object(input.base_input)),
            "payload":value(&JsValue::Object(input.payload.expect("#743 session matrix payload"))),
            "matchQuery":input.match_query.expect("#743 session matrix match").to_utf8().expect("#743 session matrix match utf8"),
            "aborted":input.signal.expect("#743 session matrix signal").is_cancelled()}));
        let action = self.action.clone();
        let gate = self.gate.clone();
        Box::pin(async move {
            if (event == "SessionStart" && action == "reject-start")
                || (event == "Setup" && action == "reject-setup")
                || (event == "SessionEnd" && action == "reject-end")
            {
                return Err(ModuleError::new(format!("{event}_DISPATCH_REJECT")).into());
            }
            if (event == "SessionStart" && action == "abort-start")
                || (event == "SessionEnd" && action == "abort-end")
            {
                gate.wait().await;
            }
            Ok(LifecycleDispatchResult::default())
        })
    }
}
struct Runner {
    action: String,
    signal: Cancellation,
    gate: Arc<Gate>,
    emitted: bool,
}
impl LiveTurn for Runner {
    fn next(&mut self) -> SessionNext<'_> {
        Box::pin(async move {
            if self.emitted {
                return Ok(None);
            }
            if self.action == "abort-turn" || self.action == "abort-throw" {
                self.gate.wait().await;
            }
            self.emitted = true;
            if self.action == "throw" || self.action == "abort-throw" {
                return Err(RuntimeError::new("START_TURN_PORT_FAILURE"));
            }
            let kind = if self.signal.is_cancelled() {
                "aborted"
            } else if self.action == "error" {
                "error"
            } else {
                "success"
            };
            Ok(Some(pilotdeck_protocol::parse_json(&json!({"type":"turn_completed","sessionId":"session-743","turnId":"turn-1","result":{"type":kind,"usage":{},"permissionDenials":[]}}).to_string()).expect("#743 session matrix terminal")))
        })
    }
}
fn harness(
    action: &str,
) -> (
    super::super::live::LiveSessionAdapter,
    SessionStream,
    Arc<Gate>,
    Arc<Mutex<Vec<Value>>>,
) {
    let session = super::super::live::LiveSessionAdapter::new(
        super::super::live::test_recorder(),
        Vec::new,
        |_| Box::pin(async { Err(ModuleError::new("unused factory").into()) }),
    );
    let signal = Cancellation::new();
    {
        let mut state = lock(&session.state);
        state.status = SessionStatus::Running;
        state.current_turn_id = Some("turn-1".into());
        state.signal = signal.clone();
        state.active = Some(crate::host::Cancellation::default());
    }
    let gate = Arc::new(Gate::new());
    let calls = Arc::new(Mutex::new(vec![]));
    let shell = SessionStream {
        turn: Box::new(Runner {
            action: action.into(),
            signal,
            gate: gate.clone(),
            emitted: false,
        }),
        preparing: None,
        lifecycle: Some(SessionLifecycle::test(Arc::new(Port {
            action: action.into(),
            gate: gate.clone(),
            calls: calls.clone(),
        }))),
        pending: None,
        session: "session-743".into(),
        state: session.state.clone(),
        active: Some(Arc::new(ActiveTurn(session.state.clone()))),
        phase: Phase::Started,
        result: None,
        end_reason: "prompt_input_exit",
    };
    (session, shell, gate, calls)
}
fn snapshot(session: &super::super::live::LiveSessionAdapter) -> Value {
    let snapshot = value(&session.snapshot());
    json!({"status":snapshot["status"],"currentTurnId":snapshot.get("currentTurnId").unwrap_or(&Value::Null)})
}
async fn take(
    shell: &mut SessionStream,
    session: &super::super::live::LiveSessionAdapter,
    events: &mut Vec<Value>,
    states: &mut Vec<Value>,
    abort: bool,
    gate: &Gate,
) -> Result<bool, RuntimeError> {
    let mut next = shell.next();
    if abort {
        tokio::select! { biased;
            _=bounded(gate.entered())=>{},
            result=&mut next=>panic!("#743 session matrix completed before gate: {result:?}"),
        }
        session
            .abort(&"test-abort".into())
            .expect("#743 session matrix abort");
        // Poll once after abort: cancellation is notification, not completion.
        assert!(
            std::future::poll_fn(|cx| std::task::Poll::Ready(next.as_mut().poll(cx)))
                .await
                .is_pending(),
            "#743 session matrix hook/runner must remain held"
        );
        gate.open();
    }
    match bounded(next).await? {
        Some(event) => {
            events.push(value(&event));
            states.push(snapshot(session));
            Ok(false)
        }
        None => Ok(true),
    }
}

#[tokio::test]
async fn session_matrix_matches_ts() {
    let oracle = crate::test_baseline::oracle(include_str!(
        "../../../tests/fixtures/live-session/oracle.json"
    ));
    let oracle = value(&oracle);
    let rows = oracle["cases"]
        .as_array()
        .expect("#743 session matrix cases");
    assert_eq!(rows.len(), 23, "#743 session matrix case count");
    for row in rows {
        let action = row["action"].as_str().expect("#743 session matrix action");
        let (session, mut shell, gate, calls) = harness(action);
        let mut events = vec![];
        let mut states = vec![];
        let mut error = None;
        for step in 0..5 {
            if step == 3 && action == "abort-terminal" {
                session
                    .abort(&"test-abort".into())
                    .expect("#743 session matrix terminal abort");
            }
            let abort = (step == 1 && action == "abort-start")
                || (step == 2 && matches!(action, "abort-turn" | "abort-throw"))
                || (step == 3 && action == "abort-end");
            match take(&mut shell, &session, &mut events, &mut states, abort, &gate).await {
                Ok(true) => break,
                Ok(false) => {}
                Err(e) => {
                    error = Some(e.message);
                    break;
                }
            }
        }
        assert!(
            bounded(shell.next())
                .await
                .expect("#743 session matrix final next")
                .is_none(),
            "#743 session matrix None"
        );
        crate::negative_assert_eq!(
            json!({"events":events,"states":states,"calls":lock(&calls).clone(),"error":error,"final":snapshot(&session)}),
            json!({"events":row["events"],"states":row["states"],"calls":row["calls"],"error":row["error"],"final":row["final"]}),
            "#743 session matrix {}",
            row["name"].as_str().expect("#743 session matrix name")
        );
        assert!(
            lock(&session.state).active.is_none(),
            "#743 session matrix slot release {}",
            row["name"]
        );
    }
}

#[tokio::test]
async fn cancelled_next_preserves_hook_and_session_slot() {
    let (session, mut shell, gate, calls) = harness("abort-end");
    for _ in 0..3 {
        bounded(shell.next())
            .await
            .expect("#743 held End prefix")
            .expect("#743 held End event");
    }
    let mut next = shell.next();
    tokio::select! { _=bounded(gate.entered())=>{}, result=&mut next=>panic!("#743 held End ended early {result:?}") }
    drop(next);
    session.abort(&"late".into()).expect("#743 held End abort");
    assert!(
        lock(&session.state).active.is_some(),
        "#743 held End slot retained"
    );
    let mut next = shell.next();
    assert!(
        std::future::poll_fn(|cx| std::task::Poll::Ready(next.as_mut().poll(cx)))
            .await
            .is_pending(),
        "#743 held End still pending"
    );
    drop(next);
    gate.open();
    let event = bounded(shell.next())
        .await
        .expect("#743 held End settled")
        .expect("#743 held End session event");
    assert_eq!(
        event.get("reason"),
        Some(&"prompt_input_exit".into()),
        "#743 held End reason fixed before late abort"
    );
    assert_eq!(lock(&calls).len(), 3, "#743 held End called exactly once");
    assert!(
        lock(&session.state).active.is_some(),
        "#743 held End slot retained at session_ended"
    );
    assert!(
        bounded(shell.next())
            .await
            .expect("#743 held End None")
            .is_none(),
        "#743 held End None observed"
    );
    assert!(
        lock(&session.state).active.is_none(),
        "#743 held End releases at None"
    );
}

struct OwnedSession {
    adapter: super::super::live::LiveSessionAdapter,
    stream: Mutex<Option<SessionStream>>,
    disposed: std::sync::atomic::AtomicBool,
}
impl LiveSession for OwnedSession {
    fn submit(&self, _: JsValue, _: JsValue) -> Result<Box<dyn LiveTurn>, RuntimeError> {
        lock(&self.stream)
            .take()
            .map(|s| Box::new(s) as Box<dyn LiveTurn>)
            .ok_or_else(|| RuntimeError::new("test stream already taken"))
    }
    fn snapshot(&self) -> JsValue {
        self.adapter.snapshot()
    }
    fn abort(&self, reason: &JsString) -> crate::runtime::project::RuntimeResult {
        self.adapter.abort(reason)
    }
    fn dispose(&self) -> crate::runtime::project::DisposeFuture<'_> {
        self.disposed
            .store(true, std::sync::atomic::Ordering::SeqCst);
        self.adapter.dispose()
    }
}
#[tokio::test]
async fn owner_holds_end_and_releases_at_none() {
    use crate::runtime::session::LiveSessionHandle;
    for action in ["abort-end", "abort-turn"] {
        let (adapter, shell, gate, _) = harness(action);
        let session = Arc::new(OwnedSession {
            adapter,
            stream: Mutex::new(Some(shell)),
            disposed: std::sync::atomic::AtomicBool::new(false),
        });
        let handle = LiveSessionHandle::new(session.clone(), None);
        let mut submission = handle
            .submit(JsValue::Null, JsValue::Null)
            .expect("#743 owner submit");
        for _ in 0..if action == "abort-end" { 3 } else { 2 } {
            bounded(submission.next())
                .await
                .expect("#743 owner prefix")
                .expect("#743 owner event");
        }
        let mut next = Box::pin(submission.next());
        tokio::select! {_=bounded(gate.entered())=>{},result=&mut next=>panic!("#743 owner End pending {result:?}")}
        let busy = handle
            .submit(JsValue::Null, JsValue::Null)
            .err()
            .expect("#743 owner busy during End");
        crate::negative_assert_eq!(
            busy.message,
            "Agent handle already has an active turn.",
            "#743 owner busy during End"
        );
        let mut dispose = handle.dispose("owner-closed".into());
        assert!(
            std::future::poll_fn(|cx| std::task::Poll::Ready(dispose.as_mut().poll(cx)))
                .await
                .is_pending(),
            "#743 owner dispose waits End"
        );
        assert!(
            !session.disposed.load(std::sync::atomic::Ordering::SeqCst),
            "#743 owner no early session dispose"
        );
        gate.open();
        let first = bounded(next)
            .await
            .expect("#743 owner gated result")
            .expect("#743 owner gated event");
        if action == "abort-turn" {
            assert_eq!(
                first.get("type"),
                Some(&"turn_completed".into()),
                "#743 owner terminal before End"
            );
            assert!(
                !session.disposed.load(std::sync::atomic::Ordering::SeqCst),
                "#743 owner no dispose at terminal"
            );
        }
        let end = if action == "abort-turn" {
            bounded(submission.next())
                .await
                .expect("#743 owner End result")
                .expect("#743 owner End event")
        } else {
            first
        };
        assert_eq!(
            end.get("type"),
            Some(&"session_ended".into()),
            "#743 owner session_ended"
        );
        assert!(
            !session.disposed.load(std::sync::atomic::Ordering::SeqCst),
            "#743 owner no dispose at session_ended"
        );
        assert!(
            bounded(submission.next())
                .await
                .expect("#743 owner None result")
                .is_none(),
            "#743 owner None"
        );
        crate::test_negative_receipt::expect_ok(
            timeout(Duration::from_secs(5), dispose).await,
            "#743 owner release on None",
        )
        .expect("#743 owner disposal result");
        crate::negative_assert!(
            session.disposed.load(std::sync::atomic::Ordering::SeqCst),
            "#743 owner release on None"
        );
    }
}
#[tokio::test]
async fn ordinary_hook_results_keep_session_tail() {
    use crate::hooks::protocol::{HookCommand, HookMatcher};
    use crate::runtime::hooks::exec::{
        CallbackFuture, CallbackHookHandler, CallbackInput, CallbackResult,
    };
    use crate::runtime::hooks::{HookRuntime, LifecycleRuntime};
    struct Callback(bool, Arc<std::sync::atomic::AtomicUsize>);
    impl CallbackHookHandler for Callback {
        fn call(&self, _: CallbackInput) -> CallbackFuture {
            let block = self.0;
            self.1.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Box::pin(async move {
                if block {
                    Ok(CallbackResult::Output(
                        crate::hooks::protocol::HookOutput::Sync(Box::new(
                            crate::hooks::protocol::HookSyncOutput {
                                continue_: Some(false),
                                ..Default::default()
                            },
                        )),
                    ))
                } else {
                    Err("HOOK_FAILURE".into())
                }
            })
        }
    }
    for event in ["SessionStart", "Setup", "SessionEnd"] {
        for block in [false, true] {
            let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let runtime = HookRuntime::new(vec![(
                event.into(),
                vec![HookMatcher {
                    hooks: vec![HookCommand::Callback {
                        name: "test".into(),
                        condition: None,
                        status_message: None,
                    }],
                    ..Default::default()
                }],
            )]);
            let _registration = runtime
                .callback_executor()
                .register(&"test".into(), Arc::new(Callback(block, calls.clone())))
                .expect("#743 executor callback");
            let (session, mut shell, _, _) = harness("");
            shell.lifecycle = Some(SessionLifecycle::test(Arc::new(LifecycleRuntime::new(
                runtime,
            ))));
            let mut events = vec![];
            while let Some(event) = crate::test_negative_receipt::expect_ok(
                bounded(shell.next()).await,
                "#743 executor returned errors ignored",
            ) {
                events.push(value(&event)["type"].clone());
            }
            crate::negative_assert_eq!(
                events,
                json!([
                    "session_started",
                    "setup_completed",
                    "turn_completed",
                    "session_ended"
                ])
                .as_array()
                .expect("#743 executor expected array")
                .clone(),
                "#743 executor returned errors ignored"
            );
            assert_eq!(
                calls.load(std::sync::atomic::Ordering::SeqCst),
                1,
                "#743 executor callback actually ran"
            );
            assert_eq!(snapshot(&session)["status"], "idle", "#743 executor status");
        }
    }
}

#[tokio::test]
async fn dispose_running_signals_then_closes_writer() {
    let recorder = super::super::live::test_recorder();
    let session = super::super::live::LiveSessionAdapter::new(recorder.clone(), Vec::new, |_| {
        Box::pin(async { Err(ModuleError::new("factory must stay lazy").into()) })
    });
    let options = pilotdeck_protocol::parse_json(r#"{"sessionId":"session","turnId":"turn"}"#)
        .expect("#743 dispose options");
    let input = pilotdeck_protocol::parse_json(r#"{"type":"text","text":"hello"}"#)
        .expect("#743 dispose input");
    let mut turn = session.submit(input, options).expect("#743 dispose submit");
    assert_eq!(
        bounded(turn.next())
            .await
            .expect("#743 dispose started")
            .expect("#743 dispose event")
            .get("type"),
        Some(&"session_started".into()),
        "#743 dispose first event"
    );
    let mut invalid = JsObject::new();
    invalid.insert("type".into(), "step_started".into());
    let open_error = recorder
        .writer()
        .record_session_event(&"session".into(), &"turn".into(), invalid.clone())
        .expect_err("#743 open writer must reject step outside turn");
    assert!(
        open_error
            .to_string()
            .contains("step_started requires an active turn turn."),
        "#743 open writer must reach domain validation: {open_error}"
    );
    bounded(session.dispose())
        .await
        .expect("#743 dispose running");
    crate::negative_assert!(
        lock(&session.state).signal.is_cancelled(),
        "#743 dispose aborts running signal"
    );
    assert_eq!(
        snapshot(&session)["status"],
        "aborted",
        "#743 dispose status"
    );
    // The same input was rejected by domain validation before close. This
    // distinguishes a closed writer from an open one without relying on I/O.
    crate::test_negative_receipt::expect_ok(
        recorder
            .writer()
            .record_session_event(&"session".into(), &"turn".into(), invalid),
        "#743 dispose closes writer",
    );
    let mut kinds = vec![];
    while let Some(event) = bounded(turn.next()).await.expect("#743 dispose drain") {
        kinds.push(value(&event)["type"].clone());
    }
    assert_eq!(
        kinds,
        json!(["setup_completed", "turn_completed", "session_ended"])
            .as_array()
            .expect("#743 dispose expected")
            .clone(),
        "#743 dispose drain to None"
    );
}

#[tokio::test]
async fn lifecycle_drop_off_runtime_uses_captured_executor() {
    struct Close(Cancellation);
    impl LifecyclePort for Close {
        fn dispatch(&self, _: LifecycleDispatchInput) -> HostFuture<LifecycleDispatchResult> {
            Box::pin(async { Ok(LifecycleDispatchResult::default()) })
        }
        fn dispose(&self) -> HostFuture<()> {
            self.0.cancel();
            Box::pin(async { Ok(()) })
        }
    }
    let registry = crate::runtime::project::ProjectRuntimeRegistry::new(
        std::env::temp_dir(),
        Arc::new(|_: &std::path::Path, _: &mut crate::runtime::project::ResourceBundle| Ok(())),
    );
    let lease = Arc::new(registry.acquire(None).expect("#743 off-runtime lease"));
    let closed = Cancellation::new();
    let mut lifecycle = SessionLifecycle::test(Arc::new(Close(closed.clone())));
    lifecycle.owned = Some(SessionOwnership {
        lease,
        contribution: None,
        turn: None,
    });
    let dropped = std::thread::spawn(move || drop(lifecycle)).join();
    crate::negative_assert!(
        dropped.is_ok(),
        "#743 off-runtime lifecycle drop cannot panic"
    );
    bounded(closed.cancelled()).await;
    bounded(registry.dispose())
        .await
        .expect("#743 off-runtime registry drain");
}

fn shutdown_runtime(multi_thread: bool) -> tokio::runtime::Runtime {
    let mut builder = if multi_thread {
        let mut builder = tokio::runtime::Builder::new_multi_thread();
        builder.worker_threads(1);
        builder
    } else {
        tokio::runtime::Builder::new_current_thread()
    };
    builder
        .enable_all()
        .build()
        .expect("#743 shutdown runtime builds")
}

fn shutdown_stream() -> (SessionStream, Arc<crate::runtime::project::ProjectRuntime>) {
    let (_, mut stream, _, _) = harness("");
    let registry = crate::runtime::project::ProjectRuntimeRegistry::new(
        std::env::temp_dir(),
        Arc::new(|_: &std::path::Path, _: &mut crate::runtime::project::ResourceBundle| Ok(())),
    );
    let lease = Arc::new(registry.acquire(None).expect("#743 shutdown lease"));
    let project = lease.runtime().clone();
    stream
        .lifecycle
        .as_mut()
        .expect("#743 shutdown lifecycle")
        .owned = Some(SessionOwnership {
        lease,
        contribution: None,
        turn: None,
    });
    (stream, project)
}

#[test]
fn lifecycle_shutdown_drop_settles_without_unwind() {
    use std::panic::{AssertUnwindSafe, catch_unwind};
    for multi_thread in [false, true] {
        // Runtime shutdown cannot poll the new cleanup task. Both the stream's
        // Drop path and explicit close must release ownership synchronously.
        let runtime = shutdown_runtime(multi_thread);
        let ((stream, project), lifecycle) = runtime.block_on(async {
            let (mut closing, _) = shutdown_stream();
            (
                shutdown_stream(),
                closing.lifecycle.take().expect("#743 shutdown close owner"),
            )
        });
        drop(runtime);
        assert!(
            catch_unwind(AssertUnwindSafe(|| drop(stream))).is_ok(),
            "#743 shutdown SessionStream drop cannot unwind"
        );
        assert_eq!(
            project.lease_count(),
            0,
            "#743 shutdown drops unpolled lease"
        );
        let close = catch_unwind(AssertUnwindSafe(|| lifecycle.close()))
            .expect("#743 shutdown close cannot unwind");
        let observer = shutdown_runtime(false);
        let error = observer
            .block_on(bounded(close))
            .expect_err("#743 shutdown cleanup must report cancellation");
        crate::negative_assert_eq!(
            error.message,
            "Interaction cleanup task was dropped or panicked before completion.",
            "#743 shutdown completion guard settles unpolled cleanup"
        );

        // The predecessor deliberately never completes. The entered signal
        // proves the runtime owns the stream before shutdown cancels that task.
        let runtime = shutdown_runtime(multi_thread);
        let (task, project) = runtime.block_on(async {
            let (stream, project) = shutdown_stream();
            let (entered, ready) = oneshot::channel();
            let task = tokio::spawn(async move {
                let _stream = stream;
                entered.send(()).expect("#743 shutdown entered receiver");
                std::future::pending::<()>().await;
            });
            bounded(ready).await.expect("#743 shutdown task entered");
            (task, project)
        });
        assert_eq!(
            project.lease_count(),
            1,
            "#743 shutdown pending owner retains lease"
        );
        assert!(
            catch_unwind(AssertUnwindSafe(|| drop(runtime))).is_ok(),
            "#743 shutdown cancellation cannot unwind"
        );
        assert!(task.is_finished(), "#743 shutdown cancels pending owner");
        assert_eq!(
            project.lease_count(),
            0,
            "#743 shutdown cancelled owner lease"
        );
        let error = observer
            .block_on(bounded(task))
            .expect_err("#743 shutdown pending owner must not complete");
        assert!(
            error.is_cancelled(),
            "#743 shutdown owner cancelled, not panicked"
        );
    }
}

#[test]
fn lifecycle_close_completion_covers_error_panic_and_cancellation() {
    struct Closing {
        mode: &'static str,
        entered: Cancellation,
    }
    impl LifecyclePort for Closing {
        fn dispatch(&self, _: LifecycleDispatchInput) -> HostFuture<LifecycleDispatchResult> {
            Box::pin(async { Ok(LifecycleDispatchResult::default()) })
        }
        fn dispose(&self) -> HostFuture<()> {
            let mode = self.mode;
            let entered = self.entered.clone();
            Box::pin(async move {
                entered.cancel();
                match mode {
                    "error" => Err(ModuleError::new("#743 disposer rejected").into()),
                    "panic" => panic!("#743 injected disposer panic"),
                    "pending" => std::future::pending().await,
                    _ => Ok(()),
                }
            })
        }
    }
    for mode in ["success", "error", "panic", "pending"] {
        let runtime = shutdown_runtime(false);
        let entered = Cancellation::new();
        let close = {
            let _entered_runtime = runtime.enter();
            SessionLifecycle::test(Arc::new(Closing {
                mode,
                entered: entered.clone(),
            }))
            .close()
        };
        runtime.block_on(bounded(entered.cancelled()));
        // The pending disposer has actually been polled. Shutdown, rather than
        // a time delay, cancels it; observing completion uses a live executor.
        drop(runtime);
        let result = shutdown_runtime(false).block_on(bounded(close));
        match mode {
            "success" => result.expect("#743 completion preserves success"),
            "error" => crate::negative_assert_eq!(
                result
                    .expect_err("#743 completion preserves rejection")
                    .message,
                "Callback (accepted=false, stream_id=None, last_applied_sequence=None): #743 disposer rejected",
                "#743 completion preserves disposer error"
            ),
            _ => crate::negative_assert_eq!(
                result
                    .expect_err("#743 completion reports task loss")
                    .message,
                "Interaction cleanup task was dropped or panicked before completion.",
                "#743 completion reports panicked or cancelled cleanup"
            ),
        }
    }
}

#[tokio::test]
async fn actual_callbacks_settle_on_their_own_completion() {
    use crate::hooks::protocol::{HookCommand, HookMatcher};
    use crate::runtime::hooks::{
        HookRuntime, LifecycleRuntime,
        exec::{CallbackFuture, CallbackHookHandler, CallbackInput},
    };
    struct Held {
        cooperative: bool,
        gate: Arc<Gate>,
    }
    impl CallbackHookHandler for Held {
        fn call(&self, input: CallbackInput) -> CallbackFuture {
            let gate = self.gate.clone();
            let cooperative = self.cooperative;
            Box::pin(async move {
                gate.entered.add_permits(1);
                if cooperative {
                    input
                        .signal
                        .expect("#743 callback signal")
                        .cancelled()
                        .await;
                } else {
                    gate.release
                        .acquire()
                        .await
                        .expect("#743 callback release")
                        .forget();
                }
                Err("CALLBACK_CANCELLED".into())
            })
        }
    }
    for event in ["SessionStart", "SessionEnd"] {
        for cooperative in [false, true] {
            let (session, mut shell, gate, _) = harness("");
            let runtime = HookRuntime::new(vec![(
                event.into(),
                vec![HookMatcher {
                    hooks: vec![HookCommand::Callback {
                        name: "held".into(),
                        condition: None,
                        status_message: None,
                    }],
                    ..Default::default()
                }],
            )]);
            let _registration = runtime
                .callback_executor()
                .register(
                    &"held".into(),
                    Arc::new(Held {
                        cooperative,
                        gate: gate.clone(),
                    }),
                )
                .expect("#743 callback registration");
            shell.lifecycle = Some(SessionLifecycle::test(Arc::new(LifecycleRuntime::new(
                runtime,
            ))));
            let mut events = vec![];
            for _ in 0..if event == "SessionStart" { 1 } else { 3 } {
                events.push(value(
                    &bounded(shell.next())
                        .await
                        .expect("#743 callback prefix")
                        .expect("#743 callback event"),
                ));
            }
            let mut next = shell.next();
            tokio::select! {_ = bounded(gate.entered()) => {}, result = &mut next => panic!("#743 callback held before abort {result:?}")}
            session
                .abort(&"user-stop".into())
                .expect("#743 callback abort");
            if !cooperative {
                crate::negative_assert!(
                    std::future::poll_fn(|cx| std::task::Poll::Ready(next.as_mut().poll(cx)))
                        .await
                        .is_pending(),
                    "#743 ignored callback remains pending after abort"
                );
                gate.open();
            }
            events.push(value(
                &bounded(next)
                    .await
                    .expect("#743 callback settles")
                    .expect("#743 callback next event"),
            ));
            while let Some(event) = bounded(shell.next()).await.expect("#743 callback drain") {
                events.push(value(&event));
            }
            crate::negative_assert_eq!(
                events.iter().map(|e| e["type"].clone()).collect::<Vec<_>>(),
                json!([
                    "session_started",
                    "setup_completed",
                    "turn_completed",
                    "session_ended"
                ])
                .as_array()
                .expect("#743 callback expected")
                .clone(),
                "#743 actual callback tail reaches None"
            );
            assert_eq!(
                events[2]["result"]["type"],
                if event == "SessionStart" {
                    "aborted"
                } else {
                    "success"
                },
                "#743 actual callback terminal result"
            );
            assert_eq!(
                events[3]["reason"],
                if event == "SessionStart" {
                    "other"
                } else {
                    "prompt_input_exit"
                },
                "#743 actual callback End reason"
            );
            assert_eq!(
                snapshot(&session)["status"],
                "aborted",
                "#743 actual callback final status"
            );
        }
    }
}
