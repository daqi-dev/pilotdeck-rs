use super::*;
use crate::host::{Cancellation, HostFuture};
use pilotdeck_agent::ports::PortResult;
use pilotdeck_protocol::admission::ExecutionIdentity;
use pilotdeck_protocol::module::ModuleOutcome;
use pilotdeck_transport::server::{
    ExecuteRequest as ServerRequest, ExecutionContext, ExecutionFactory, ExecutionTerminal,
};
use std::sync::atomic::{AtomicUsize, Ordering};

#[derive(Default)]
struct Recorder {
    events: Mutex<Vec<JsValue>>,
}
impl HostHandler for Recorder {
    fn module_call(&self, _: JsValue) -> HostFuture<JsObject> {
        Box::pin(async { Ok(JsObject::new()) })
    }
    fn event(&self, event: JsValue) -> HostFuture<()> {
        self.events.lock().unwrap().push(event);
        Box::pin(async { Ok(()) })
    }
}
struct FixtureFactory {
    calls: AtomicUsize,
}
impl ExecutionFactory for FixtureFactory {
    fn execute(
        &self,
        req: &ServerRequest,
        ctx: &mut ExecutionContext,
    ) -> PortResult<ExecutionTerminal> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if req.run_id.equals_str("slow") {
            std::thread::sleep(Duration::from_millis(100));
        }
        for i in 0..3 {
            let mut payload = JsObject::new();
            payload.insert("type".into(), "fixture_event".into());
            payload.insert("run".into(), JsValue::String(req.run_id.clone()));
            payload.insert("index".into(), JsValue::Number(i as f64));
            ctx.emit("fixture".into(), payload)?;
        }
        Ok(ExecutionTerminal {
            outcome: ModuleOutcome::Completed,
            payload: JsObject::new(),
            error: None,
        })
    }
}
fn fixture() -> SessionEmbeddedWorker {
    let owner = ConnectionOwner::new(
        SidecarBinding {
            module_id: "pilotdeck-agent-loop".into(),
            module_instance_id: "fixture".into(),
            connection_generation: "fixture-generation".into(),
            capabilities_version: "1".into(),
        },
        QUEUE,
    );
    let server = ExecutionServer::new(
        owner.clone(),
        Arc::new(FixtureFactory {
            calls: AtomicUsize::new(0),
        }),
        Arc::new(SystemExecutionClock),
        QUEUE,
    );
    let worker_server = server.clone();
    let worker = std::thread::spawn(move || worker_server.worker());
    SessionEmbeddedWorker {
        inner: Mutex::new(Some(EmbeddedRuntime {
            server,
            owner,
            worker,
            reusable: true,
        })),
        cwd: "/tmp".into(),
        execution: tokio::sync::Mutex::new(()),
    }
}
fn request(run: &str) -> ExecuteRequest {
    ExecuteRequest {
        identity: ExecutionIdentity {
            run_id: run.into(),
            operation_id: format!("op-{run}").into(),
            request_id: format!("req-{run}").into(),
            session_id: "session".into(),
            turn_id: format!("turn-{run}").into(),
        },
        payload: JsObject::new(),
        idempotency_key: None,
        operation_deadline: None,
    }
}
fn limits() -> Limits {
    Limits {
        execution_timeout: Some(Duration::from_secs(2)),
        terminal_drain_timeout: Duration::from_millis(1),
        ..Limits::default()
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn hot_worker_survives_more_than_control_queue_capacity_and_delivers_last_event() {
    let worker = fixture();
    for turn in 0..(QUEUE + 8) {
        let handler = Arc::new(Recorder::default());
        let run = format!("run-{turn}");
        let result = worker
            .execute(
                request(&run),
                handler.clone(),
                limits(),
                Cancellation::default(),
            )
            .await
            .unwrap();
        assert!(
            matches!(result, ExecuteResult::Terminal(_)),
            "turn {turn}: {result:?}"
        );
        let events = handler.events.lock().unwrap();
        assert_eq!(
            events.len(),
            3,
            "turn {turn} must deliver every event before returning"
        );
        for (index, event) in events.iter().enumerate() {
            assert_eq!(event.get("run"), Some(&JsValue::from(run.as_str())));
            assert_eq!(event.get("index"), Some(&JsValue::Number(index as f64)));
        }
    }
    worker.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn expired_control_rejects_without_waiting_for_stream_terminal() {
    let worker = fixture();
    let mut expired = request("expired");
    expired.operation_deadline = Some("2000-01-01T00:00:00.000Z".into());
    let started = std::time::Instant::now();
    let result = worker
        .execute(
            expired,
            Arc::new(Recorder::default()),
            limits(),
            Cancellation::default(),
        )
        .await
        .unwrap();
    assert!(matches!(result, ExecuteResult::Rejected(_)), "{result:?}");
    assert!(started.elapsed() < Duration::from_secs(1));
    let next = worker
        .execute(
            request("after-expired"),
            Arc::new(Recorder::default()),
            limits(),
            Cancellation::default(),
        )
        .await
        .unwrap();
    assert!(matches!(next, ExecuteResult::Terminal(_)));
    worker.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn timeout_retires_old_drain_and_does_not_deliver_old_events_to_next_turn() {
    let worker = fixture();
    let old_owner = worker.inner.lock().unwrap().as_ref().unwrap().owner.clone();
    let old = Arc::new(Recorder::default());
    let result = worker
        .execute(
            request("slow"),
            old.clone(),
            Limits {
                execution_timeout: Some(Duration::from_millis(10)),
                ..limits()
            },
            Cancellation::default(),
        )
        .await;
    assert!(
        result.is_err(),
        "slow turn should hit its local execution limit"
    );
    let next = Arc::new(Recorder::default());
    let result = worker
        .execute(
            request("next"),
            next.clone(),
            limits(),
            Cancellation::default(),
        )
        .await
        .unwrap();
    assert!(matches!(
        result,
        ExecuteResult::Rejected(_) | ExecuteResult::Terminal(_)
    ));
    assert!(old_owner.is_closed());
    let new_owner = worker.inner.lock().unwrap().as_ref().unwrap().owner.clone();
    assert_ne!(
        old_owner.binding().connection_generation,
        new_owner.binding().connection_generation
    );
    for event in next.events.lock().unwrap().iter() {
        assert_ne!(event.get("run"), Some(&JsValue::from("slow")));
    }
    let count = old.events.lock().unwrap().len();
    tokio::time::sleep(Duration::from_millis(30)).await;
    assert_eq!(
        old.events.lock().unwrap().len(),
        count,
        "timed-out handler must have no detached drain"
    );
    worker.shutdown().await;
}

#[tokio::test(flavor = "current_thread")]
async fn dropping_execute_retires_callbacks_and_shutdown_does_not_block_runtime() {
    let worker = fixture();
    let owner = worker.inner.lock().unwrap().as_ref().unwrap().owner.clone();
    let handler = Arc::new(Recorder::default());
    let result = tokio::time::timeout(
        Duration::from_millis(10),
        worker.execute(
            request("slow"),
            handler.clone(),
            limits(),
            Cancellation::default(),
        ),
    )
    .await;
    assert!(
        result.is_err(),
        "outer timeout must drop the execute future"
    );
    tokio::time::timeout(Duration::from_secs(1), worker.shutdown())
        .await
        .unwrap();
    assert!(owner.is_closed());
    let count = handler.events.lock().unwrap().len();
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert_eq!(
        handler.events.lock().unwrap().len(),
        count,
        "dropped turn must stop callbacks"
    );
}

struct PendingHandler {
    entered: Arc<tokio::sync::Notify>,
    release: Arc<tokio::sync::Notify>,
}
impl HostHandler for PendingHandler {
    fn module_call(&self, _: JsValue) -> HostFuture<JsObject> {
        Box::pin(async { Ok(JsObject::new()) })
    }
    fn event(&self, _: JsValue) -> HostFuture<()> {
        let entered = self.entered.clone();
        let release = self.release.clone();
        Box::pin(async move {
            entered.notify_one();
            release.notified().await;
            Ok(())
        })
    }
}
struct CancellationFactory {
    observed: Arc<std::sync::atomic::AtomicBool>,
}
impl ExecutionFactory for CancellationFactory {
    fn execute(
        &self,
        _: &ServerRequest,
        ctx: &mut ExecutionContext,
    ) -> PortResult<ExecutionTerminal> {
        let mut payload = JsObject::new();
        payload.insert("type".into(), "pending".into());
        ctx.emit("pending".into(), payload)?;
        let end = std::time::Instant::now() + Duration::from_secs(2);
        while std::time::Instant::now() < end {
            if ctx.control.check().is_err() {
                self.observed.store(true, Ordering::SeqCst);
                return ctx.control.check().map(|_| unreachable!());
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        panic!("host cancellation was not forwarded while event callback was pending");
    }
}
#[tokio::test(flavor = "current_thread")]
async fn pending_event_callback_does_not_block_cancellation_forwarding() {
    let observed = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let owner = ConnectionOwner::new(
        SidecarBinding {
            module_id: "pilotdeck-agent-loop".into(),
            module_instance_id: "pending-fixture".into(),
            connection_generation: "pending-generation".into(),
            capabilities_version: "1".into(),
        },
        QUEUE,
    );
    let server = ExecutionServer::new(
        owner.clone(),
        Arc::new(CancellationFactory {
            observed: observed.clone(),
        }),
        Arc::new(SystemExecutionClock),
        QUEUE,
    );
    let worker_server = server.clone();
    let thread = std::thread::spawn(move || worker_server.worker());
    let worker = SessionEmbeddedWorker {
        inner: Mutex::new(Some(EmbeddedRuntime {
            server,
            owner,
            worker: thread,
            reusable: true,
        })),
        cwd: "/tmp".into(),
        execution: tokio::sync::Mutex::new(()),
    };
    let entered = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let cancel = Cancellation::default();
    let execution = worker.execute(
        request("pending"),
        Arc::new(PendingHandler {
            entered: entered.clone(),
            release: release.clone(),
        }),
        limits(),
        cancel.clone(),
    );
    tokio::pin!(execution);
    tokio::select! { result = &mut execution => panic!("execution ended early: {result:?}"), _ = entered.notified() => {} }
    cancel.cancel(None);
    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            tokio::select! { result = &mut execution => panic!("callback was skipped: {result:?}"), _ = tokio::time::sleep(Duration::from_millis(1)) => {} }
            if observed.load(Ordering::SeqCst) { break; }
        }
    }).await.unwrap();
    release.notify_one();
    let result = tokio::time::timeout(Duration::from_secs(1), &mut execution)
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(
        result,
        ExecuteResult::Rejected(_) | ExecuteResult::Terminal(_)
    ));
    worker.shutdown().await;
}

struct PendingModuleHandler {
    entered: Arc<tokio::sync::Notify>,
    dropped: Arc<std::sync::atomic::AtomicBool>,
}
struct DropMark(Arc<std::sync::atomic::AtomicBool>);
impl Drop for DropMark {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}
impl HostHandler for PendingModuleHandler {
    fn module_call(&self, _: JsValue) -> HostFuture<JsObject> {
        let entered = self.entered.clone();
        let dropped = self.dropped.clone();
        Box::pin(async move {
            let _mark = DropMark(dropped);
            entered.notify_one();
            std::future::pending::<Result<JsObject, crate::host::HostError>>().await
        })
    }
}
#[tokio::test(flavor = "current_thread")]
async fn direct_host_cancellation_drops_pending_callback_on_current_thread_runtime() {
    use pilotdeck_agent::ports::{CallControl, CancelCause, PortError};
    use pilotdeck_transport::direct_host::DirectHostCallback;
    let entered = Arc::new(tokio::sync::Notify::new());
    let dropped = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let callback = DirectModuleHost::new(
        Arc::new(PendingModuleHandler {
            entered: entered.clone(),
            dropped: dropped.clone(),
        }),
        tokio::runtime::Handle::current(),
    );
    let control = CallControl {
        cancellation: Default::default(),
        deadline: None,
    };
    let worker_control = control.clone();
    let (tx, mut rx) = tokio::sync::oneshot::channel();
    let thread = std::thread::spawn(move || {
        let result = callback.dispatch_module_call(&worker_control, &JsValue::Null);
        let _ = tx.send(result);
    });
    tokio::time::timeout(Duration::from_secs(1), entered.notified())
        .await
        .unwrap();
    control.cancellation.cancel(CancelCause::OwnerRetired);
    let result = tokio::time::timeout(Duration::from_secs(1), &mut rx)
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(
        result,
        Err(PortError::Cancelled {
            cause: CancelCause::OwnerRetired
        })
    ));
    assert!(dropped.load(Ordering::SeqCst));
    thread.join().unwrap();
}
