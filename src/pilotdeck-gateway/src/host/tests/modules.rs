use super::*;
use crate::host::{
    execution::module_response,
    modules::{
        ModuleRouter, capability, context, event, lifecycle, permission, tool_context, wire,
    },
};

// The TS X0 fixture installs an execution port whose catalog is empty. This
// differs from a host that forgot to install the required capability port.
struct EmptyCapabilityPort;
impl capability::ToolCatalog for EmptyCapabilityPort {
    fn list(&self) -> HostFuture<Vec<Arc<dyn capability::ToolDefinition>>> {
        Box::pin(async { Ok(Vec::new()) })
    }
}
impl capability::ToolContext for EmptyCapabilityPort {
    fn prepare(&self, _call: JsValue) -> HostFuture<capability::ToolInvocationContext> {
        Box::pin(async {
            Ok(capability::ToolInvocationContext {
                runtime: Arc::new(()),
                execution: Arc::new(()),
            })
        })
    }
}
impl capability::ToolExecution for EmptyCapabilityPort {
    fn execute_all(
        &self,
        _calls: Vec<capability::ToolCall>,
        _context: capability::ToolInvocationContext,
    ) -> HostFuture<Vec<JsObject>> {
        Box::pin(async { panic!("#978 X0 calls must fail validation before execution") })
    }
}
impl capability::CapabilityResultObserver for EmptyCapabilityPort {
    fn on_capability_results(&self, _results: Vec<JsObject>) -> HostFuture<()> {
        Box::pin(async { panic!("#978 X0 calls must not reach the observer") })
    }
}
// X0 registers every optional module; only their ports stay empty, so every
// operation reports "does not support" instead of the unregistered-module
// error an absent module would answer with.
struct PanicEnvironment;
impl tool_context::TurnEnvironment for PanicEnvironment {
    fn build(
        &self,
        _: Option<&JsValue>,
        _: &JsString,
        _: &JsString,
        _: &JsString,
    ) -> Result<JsObject, HostError> {
        panic!("#978 X0 context calls must fail before environment construction")
    }
}
struct PanicPermission;
impl capability::ToolCatalog for PanicPermission {
    fn list(&self) -> HostFuture<Vec<Arc<dyn capability::ToolDefinition>>> {
        Box::pin(async { panic!("#978 X0 permission calls must fail before the catalog") })
    }
}
impl permission::PermissionContext for PanicPermission {
    fn prepare(&self, _value: JsValue) -> HostFuture<capability::RuntimeContext> {
        Box::pin(async { panic!("#978 X0 permission calls must fail before the context") })
    }
}
impl permission::PermissionDecision for PanicPermission {
    fn decide(
        &self,
        _tool: Arc<dyn capability::ToolDefinition>,
        _input: JsValue,
        _context: capability::RuntimeContext,
        _tool_call_id: JsString,
    ) -> HostFuture<JsValue> {
        Box::pin(async { panic!("#978 X0 permission calls must fail before the decision") })
    }
}
struct PanicSink;
impl event::EventSink for PanicSink {
    fn emit(&self, _event: JsObject) -> Result<(), HostError> {
        panic!("#978 X0 event calls must fail validation before the sink")
    }
}
fn fixture_router() -> ModuleRouter {
    let port = Arc::new(EmptyCapabilityPort);
    let mut input = JsObject::new();
    input.insert("sessionId".into(), "session".into());
    input.insert("turnId".into(), "turn".into());
    let binding = tool_context::TurnBinding {
        config: JsObject::new(),
        input,
        checkpoint: tool_context::HostToolCheckpoint::default(),
        abort_signal: None,
        environment: Arc::new(PanicEnvironment),
    };
    let permission_port = Arc::new(PanicPermission);
    ModuleRouter {
        capability: capability::Handler::new(port.clone(), port.clone(), port),
        permission: permission::Handler::new(
            permission_port.clone(),
            permission_port.clone(),
            permission_port,
        ),
        context: context::Handler::without_runtime(
            tool_context::ContextRequestIdentity::new(
                binding,
                tool_context::ToolContextPorts::default(),
            ),
            JsObject::new(),
            None,
        ),
        lifecycle: lifecycle::Handler::null(lifecycle::Binding {
            session_id: "session".into(),
            turn_id: "turn".into(),
            cwd: "/fixture".into(),
            permission_mode: None,
            base_env: None,
            signal: None,
        }),
        event: event::Handler::new(
            event::TurnIdentity::new("session", "turn"),
            Arc::new(PanicSink),
        ),
        ..ModuleRouter::default()
    }
}

fn oracle() -> JsValue {
    let value = std::panic::catch_unwind(|| {
        crate::test_baseline::oracle(include_str!(
            "../../../tests/fixtures/host-modules/oracle.json"
        ))
    })
    .expect("#978 routing oracle baseline");
    assert_eq!(
        array(&value, "routing").len(),
        863,
        "#978 routing oracle cardinality"
    );
    assert_eq!(
        array(&value, "errors").len(),
        11,
        "#978 errors oracle cardinality"
    );
    assert_eq!(
        array(&value, "fields").len(),
        90,
        "#978 fields oracle cardinality"
    );
    value
}
fn array<'a>(value: &'a JsValue, key: &str) -> &'a [JsValue] {
    let JsValue::Array(items) = value.get(key).expect("#978 oracle array field") else {
        panic!("#978 oracle array type: {key}")
    };
    items
}
fn nonfinite(value: &JsValue) -> Option<f64> {
    match value
        .get("nonfinite")?
        .as_string()?
        .to_utf8()
        .expect("#978 error fixture UTF-8")
        .as_str()
    {
        "NaN" => Some(f64::NAN),
        "Infinity" => Some(f64::INFINITY),
        "-Infinity" => Some(f64::NEG_INFINITY),
        other => panic!("#978 unexpected special number {other}"),
    }
}
fn module_error(case: &JsValue) -> HostError {
    let mut fields = case
        .get("fields")
        .expect("#978 error fields")
        .as_object()
        .expect("#978 error fields object")
        .clone();
    if let Some(value) = nonfinite(case) {
        fields.insert("retryAfterMs".into(), JsValue::Number(value));
    }
    ModuleError::from_js_fields(
        case.get("message")
            .expect("#978 error message")
            .as_string()
            .expect("#978 error message string")
            .clone(),
        &JsValue::Object(fields),
    )
    .into()
}
#[tokio::test]
async fn real_ts_module_routing_and_error_projection() {
    let oracle = oracle();
    let router = fixture_router();
    for case in array(&oracle, "routing") {
        let call = case.get("call").unwrap();
        let response = module_response(call, router.module_call(call.clone()).await);
        assert_eq!(&response, case.get("response").unwrap(), "call={call:?}");
    }
    for case in array(&oracle, "errors") {
        assert_eq!(
            &module_response(case.get("call").unwrap(), Err(module_error(case))),
            case.get("response").unwrap()
        );
    }
    // Generic transport/callback errors keep their old fallback shape.
    let call = array(&oracle, "errors")[0].get("call").unwrap();
    let projected = module_response(call, Err(HostError::new(HostErrorKind::Callback, "plain")));
    assert!(text_is(projected.get("code"), "HOST_MODULE_FAILED"));
    assert_eq!(
        projected.get("error").unwrap(),
        &parse_json(r#"{"message":"plain"}"#).unwrap()
    );
    router.dispose().await.unwrap();
}
#[test]
fn real_ts_payload_readers() {
    let oracle = oracle();
    for case in array(&oracle, "fields") {
        let input = case.get("input").unwrap();
        let value = nonfinite(input)
            .map(JsValue::Number)
            .unwrap_or_else(|| input.get("value").cloned().unwrap_or(JsValue::Undefined));
        let mut object = JsObject::new();
        if input.get("absent").is_none() {
            object.insert("value".into(), value);
        }
        let record = if input.get("missing").is_some() {
            None
        } else {
            Some(&object)
        };
        let value = record.and_then(|object| object.get("value"));
        let helper = case
            .get("helper")
            .unwrap()
            .as_string()
            .unwrap()
            .to_utf8()
            .unwrap();
        let result = match helper.as_str() {
            "stringField" => {
                wire::string_field(record, "value").map(|v| Some(JsValue::String(v.clone())))
            }
            "optionalStringField" => wire::optional_string_field(record, "value")
                .map(|v| v.map(|v| JsValue::String(v.clone()))),
            "booleanField" => wire::boolean_field(record, "value").map(|v| Some(JsValue::Bool(v))),
            "positiveFinite" => {
                wire::positive_finite(value, "value").map(|v| Some(JsValue::Number(v)))
            }
            "optionalNonNegativeFinite" => {
                wire::optional_non_negative_finite(value, "value").map(|v| v.map(JsValue::Number))
            }
            other => panic!("unknown helper {other}"),
        };
        let mut actual = JsObject::new();
        match result {
            Ok(Some(value)) => {
                actual.insert("value".into(), value);
            }
            Ok(None) => {
                actual.insert("missing".into(), JsValue::Bool(true));
            }
            Err(error) => {
                actual.insert("error".into(), error.message.as_str().into());
            }
        }
        assert_eq!(
            &JsValue::Object(actual),
            case.get("expected").unwrap(),
            "{case:?}"
        );
    }
    assert_eq!(wire::optional_non_negative_finite(None, "x").unwrap(), None);
    assert!(wire::positive_finite(None, "x").is_err());
}
struct ErrorHandler;
impl HostHandler for ErrorHandler {
    fn module_call(&self, call: JsValue) -> HostFuture<JsObject> {
        Box::pin(async move {
            let oracle = oracle();
            let case = array(&oracle, "errors")
                .iter()
                .find(|case| case.get("call") == Some(&call))
                .expect("#978 known error fixture");
            Err(module_error(case))
        })
    }
}
// Routing checks can execute hundreds of callbacks. Bound stalled progress,
// rather than charging Python scheduling and every completed batch to one timer.
struct RoutingProgress {
    inner: Arc<dyn HostHandler>,
    completed: tokio::sync::watch::Sender<usize>,
}
impl HostHandler for RoutingProgress {
    fn module_call(&self, call: JsValue) -> HostFuture<JsObject> {
        let future = self.inner.module_call(call);
        let completed = self.completed.clone();
        Box::pin(async move {
            let result = future.await;
            completed.send_modify(|count| *count += 1);
            result
        })
    }
    fn drain_events(&self) -> Result<Vec<JsObject>, HostError> {
        self.inner.drain_events()
    }
    fn dispose(&self) -> HostFuture<()> {
        self.inner.dispose()
    }
    fn event(&self, event: JsValue) -> HostFuture<()> {
        self.inner.event(event)
    }
    fn validate_event(&self, event: &JsValue) -> Result<EventGuard, HostError> {
        self.inner.validate_event(event)
    }
    fn observe_transport(&self, observation: ReconnectObservation) {
        self.inner.observe_transport(observation);
    }
}
async fn routing_until_stalled<F: std::future::Future>(
    future: F,
    mut completed: tokio::sync::watch::Receiver<usize>,
) -> Result<F::Output, &'static str> {
    let idle = Duration::from_secs(10);
    let deadline = tokio::time::sleep(idle);
    tokio::pin!(future, deadline);
    let mut reporting = true;
    loop {
        tokio::select! {
            biased;
            result = &mut future => return Ok(result),
            changed = completed.changed(), if reporting => {
                if changed.is_ok() {
                    deadline.as_mut().reset(tokio::time::Instant::now() + idle);
                } else {
                    reporting = false;
                }
            }
            () = &mut deadline => return Err("#978 routing made no progress for 10 seconds"),
        }
    }
}

#[tokio::test]
async fn stdio_routes_multiple_modules_and_projects_error_details() {
    for (mode, handler) in [
        (
            "module-routing",
            Arc::new(fixture_router()) as Arc<dyn HostHandler>,
        ),
        (
            "module-errors",
            Arc::new(ErrorHandler) as Arc<dyn HostHandler>,
        ),
    ] {
        let (mut command, receipt) =
            std::panic::catch_unwind(|| peer(mode)).expect("#978 routing peer setup");
        let mut limits = protocol_limits();
        limits.execution_timeout = None;
        let client = HostClient::spawn(&mut command, None, limits)
            .await
            .expect("#978 routing peer handshake");
        let (completed, progress) = tokio::sync::watch::channel(0);
        let handler = Arc::new(RoutingProgress {
            inner: handler,
            completed,
        });
        let result = crate::test_negative_receipt::expect_ok(
            routing_until_stalled(client.execute(request(), handler), progress).await,
            "#978 routing progress watchdog",
        );
        let result = crate::test_negative_receipt::expect_ok(result, "#978 routing execute");
        crate::negative_assert!(
            matches!(result, ExecuteResult::Terminal(_)),
            "#978 routing terminal"
        );
        std::panic::catch_unwind(|| receipt.assert_pass(mode))
            .expect("#978 routing peer assertions");
    }
}

#[tokio::test(start_paused = true)]
async fn routing_progress_resets_idle_deadline() {
    let (completed, progress) = tokio::sync::watch::channel(0);
    let (finish, result) = tokio::sync::oneshot::channel();
    let producer = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_secs(9)).await;
        let handler = RoutingProgress {
            inner: Arc::new(ErrorHandler),
            completed,
        };
        let oracle = oracle();
        let call = array(&oracle, "errors")[0]
            .get("call")
            .expect("#978 error callback input")
            .clone();
        handler
            .module_call(call)
            .await
            .expect_err("#978 error callback completes");
        tokio::time::sleep(Duration::from_secs(9)).await;
        finish.send(42).expect("#978 completion receiver alive");
    });
    let output = routing_until_stalled(result, progress).await;
    crate::negative_assert_eq!(output, Ok(Ok(42)), "#978 progress renews idle deadline");
    producer.await.expect("#978 progress producer joins");
}

#[tokio::test(start_paused = true)]
async fn routing_progress_never_completed_is_bounded() {
    let (_completed, progress) = tokio::sync::watch::channel(0);
    let started = tokio::time::Instant::now();
    let result = routing_until_stalled(std::future::pending::<()>(), progress).await;
    crate::negative_assert_eq!(
        result,
        Err("#978 routing made no progress for 10 seconds"),
        "#978 stalled predecessor terminates"
    );
    assert_eq!(
        started.elapsed(),
        Duration::from_secs(10),
        "#978 original idle budget"
    );
}

#[tokio::test(start_paused = true)]
async fn routing_progress_closed_reporter_keeps_idle_bound() {
    let (completed, progress) = tokio::sync::watch::channel(0);
    drop(completed);
    let started = tokio::time::Instant::now();
    let result = routing_until_stalled(std::future::pending::<()>(), progress).await;
    assert_eq!(
        result,
        Err("#978 routing made no progress for 10 seconds"),
        "#978 closed reporter remains bounded"
    );
    crate::negative_assert_eq!(
        started.elapsed(),
        Duration::from_secs(10),
        "#978 closed reporter preserves idle budget"
    );
}

#[tokio::test(start_paused = true)]
async fn routing_progress_cancelled_predecessor_settles() {
    let (_completed, progress) = tokio::sync::watch::channel(0);
    let (finish, result) = tokio::sync::oneshot::channel::<()>();
    let producer = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_secs(9)).await;
        drop(finish);
    });
    let output = routing_until_stalled(result, progress).await;
    crate::negative_assert!(
        matches!(output, Ok(Err(_))),
        "#978 cancelled predecessor settles"
    );
    producer.await.expect("#978 cancelling producer joins");
}

#[tokio::test(start_paused = true)]
async fn routing_progress_dropped_driver_drops_predecessor() {
    let (_completed, progress) = tokio::sync::watch::channel(0);
    let (finish, result) = tokio::sync::oneshot::channel::<()>();
    let mut driver = Box::pin(routing_until_stalled(result, progress));
    let pending = std::future::poll_fn(|cx| {
        std::task::Poll::Ready(std::future::Future::poll(driver.as_mut(), cx).is_pending())
    })
    .await;
    assert!(pending, "#978 predecessor is pending before cancellation");
    drop(driver);
    crate::negative_assert!(
        finish.is_closed(),
        "#978 cancelled driver drops predecessor"
    );
}
#[tokio::test]
async fn dispose_once_after_callbacks_on_every_exit_path() {
    for mode in [
        "positive",
        "rejected",
        "identity",
        "disconnect",
        "callback-disconnect",
        "terminal-pending-callback",
        "cleanup-hang",
    ] {
        let (mut command, receipt) = peer(mode);
        // This checks disposal ordering, not the duration of child shutdown.
        // cleanup-hang must have time to record its EOF assertions before the
        // host kills it; a 200 ms grace races Python IO under CI load (#389).
        let limits = protocol_limits();
        let client = HostClient::spawn(&mut command, None, limits).await.unwrap();
        let handler = Handler::new(mode);
        let result = client.execute(request(), handler.clone()).await;
        if matches!(
            mode,
            "positive" | "rejected" | "terminal-pending-callback" | "cleanup-hang"
        ) {
            result.unwrap();
            receipt.assert_pass(mode);
        } else {
            assert!(result.is_err(), "{mode}");
        }
        assert_eq!(handler.disposed.load(Ordering::SeqCst), 1, "{mode}");
        assert_eq!(handler.calls_at_dispose.load(Ordering::SeqCst), 0, "{mode}");
    }
    let (mut command, receipt) = peer("cancel-pre-execute");
    let client = HostClient::spawn(&mut command, None, protocol_limits())
        .await
        .unwrap();
    let handler = Handler::new("cancel-pre-execute");
    let cancel = Cancellation::default();
    cancel.cancel(None);
    client
        .execute_cancellable(request(), handler.clone(), cancel)
        .await
        .unwrap();
    receipt.assert_pass("cancel-pre-execute");
    assert_eq!(handler.disposed.load(Ordering::SeqCst), 1);
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn dropped_execute_joins_callbacks_before_dispose() {
    let mut command = command("callback-timeout");
    let client = HostClient::spawn(&mut command, None, protocol_limits())
        .await
        .unwrap();
    let handler = Handler::new("callback-timeout");
    let task = tokio::spawn(client.execute(request(), handler.clone()));
    timeout(Duration::from_secs(5), async {
        while handler.active_calls.load(Ordering::SeqCst) == 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    timeout(Duration::from_secs(5), async {
        while handler.disposed.load(Ordering::SeqCst) == 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(handler.disposed.load(Ordering::SeqCst), 1);
    assert_eq!(handler.calls_at_dispose.load(Ordering::SeqCst), 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn execute_dropped_outside_runtime_still_joins_callbacks_and_disposes_once() {
    let mut command = command("callback-timeout");
    let client = HostClient::spawn(&mut command, None, protocol_limits())
        .await
        .unwrap();
    let handler = Handler::new("callback-timeout");
    let mut execution = Box::pin(client.execute(request(), handler.clone()));
    timeout(Duration::from_secs(5), async {
        tokio::select! {
            result = &mut execution => panic!("execution finished before drop: {result:?}"),
            () = async {
                while handler.active_calls.load(Ordering::SeqCst) == 0 {
                    tokio::task::yield_now().await;
                }
            } => {}
        }
    })
    .await
    .unwrap();
    std::thread::spawn(move || {
        assert!(tokio::runtime::Handle::try_current().is_err());
        drop(execution);
    })
    .join()
    .unwrap();
    timeout(Duration::from_secs(5), async {
        while handler.disposed.load(Ordering::SeqCst) == 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(handler.disposed.load(Ordering::SeqCst), 1);
    assert_eq!(handler.calls_at_dispose.load(Ordering::SeqCst), 0);
    assert_eq!(handler.active_calls.load(Ordering::SeqCst), 0);
}

struct DisposeProbe {
    started: Arc<AtomicUsize>,
    finished: Arc<AtomicUsize>,
    release: Arc<Notify>,
    mode: &'static str,
}
impl DisposeProbe {
    fn new(mode: &'static str) -> Arc<Self> {
        Arc::new(Self {
            started: Arc::new(AtomicUsize::new(0)),
            finished: Arc::new(AtomicUsize::new(0)),
            release: Arc::new(Notify::new()),
            mode,
        })
    }
}
struct DisposeFinished(Arc<AtomicUsize>);
impl Drop for DisposeFinished {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}
impl HostHandler for DisposeProbe {
    fn module_call(&self, _call: JsValue) -> HostFuture<JsObject> {
        Box::pin(async { Ok(JsObject::new()) })
    }
    fn dispose(&self) -> HostFuture<()> {
        let (started, finished, release, mode) = (
            self.started.clone(),
            self.finished.clone(),
            self.release.clone(),
            self.mode,
        );
        Box::pin(async move {
            started.fetch_add(1, Ordering::SeqCst);
            let _finished = DisposeFinished(finished);
            match mode {
                "reject" => Err(HostError::new(HostErrorKind::Callback, "cleanup rejection")),
                "panic" => panic!("controlled cleanup panic"),
                _ => {
                    release.notified().await;
                    Ok(())
                }
            }
        })
    }
}
#[tokio::test]
async fn disposer_timeout_rejection_and_panic_keep_the_terminal() {
    for mode in ["pending", "reject", "panic"] {
        let (mut command, receipt) = peer("positive");
        let mut client = HostClient::spawn(&mut command, None, protocol_limits())
            .await
            .unwrap();
        // close_timeout bounds child shutdown and disposer cleanup. Only the
        // cleanup deadline is under test here; the connection snapshotted the
        // five-second protocol grace at spawn, so shortening this copy after
        // spawn cannot SIGKILL the peer before it persists PASS (#389/#528).
        client.limits.close_timeout = Duration::from_millis(100);
        let handler = DisposeProbe::new(mode);
        let result = timeout(
            Duration::from_secs(5),
            client.execute(request(), handler.clone()),
        )
        .await
        .unwrap()
        .unwrap();
        assert!(matches!(result, ExecuteResult::Terminal(_)));
        receipt.assert_pass("positive");
        assert_eq!(handler.started.load(Ordering::SeqCst), 1, "{mode}");
        assert_eq!(handler.finished.load(Ordering::SeqCst), 1, "{mode}");
    }
}
#[tokio::test]
async fn dropping_execute_during_dispose_does_not_cancel_or_repeat_cleanup() {
    let (mut command, receipt) = peer("positive");
    let client = HostClient::spawn(&mut command, None, protocol_limits())
        .await
        .unwrap();
    let handler = DisposeProbe::new("wait");
    let task = tokio::spawn(client.execute(request(), handler.clone()));
    timeout(Duration::from_secs(5), async {
        while handler.started.load(Ordering::SeqCst) == 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    assert_eq!(handler.finished.load(Ordering::SeqCst), 0);
    handler.release.notify_one();
    timeout(Duration::from_secs(5), async {
        while handler.finished.load(Ordering::SeqCst) == 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(handler.started.load(Ordering::SeqCst), 1);
    assert_eq!(handler.finished.load(Ordering::SeqCst), 1);
    receipt.assert_pass("positive");
}

/// Regenerates the oracle from the real TS dispatcher and compares bytes.
#[test]
#[ignore = "requires PDRS_BASELINE (PilotDeck cc7c4e6 with node_modules)"]
fn host_modules_fixtures_regenerate_identically_from_ts() {
    crate::test_baseline::regenerates_identically("host-modules");
}
