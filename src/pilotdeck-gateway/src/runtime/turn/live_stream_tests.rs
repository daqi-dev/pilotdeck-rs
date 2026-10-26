use super::*;
use crate::host::{Limits, POST_CANCEL_EXIT_TIMEOUT};
use crate::runtime::turn::projector::{
    GatewayAgentEventProjectionInput, GatewayAgentEventProjector,
};
use std::{
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
    time::Duration,
};
use tokio::{process::Command, time::timeout};

/// Upper bound for the slow-consumer sync point. It must stay below the host's
/// own `POST_CANCEL_EXIT_TIMEOUT` so a genuinely stalled receive path fails as
/// a clear assertion instead of reaching the cancel exit the test is guarding.
const FINAL_SYNC_TIMEOUT: Duration = Duration::from_secs(3);

#[derive(Default)]
struct Handler {
    events: Mutex<Vec<JsValue>>,
    calls: AtomicUsize,
    disposed: AtomicUsize,
    fail_event: bool,
    /// Each callback synchronously captures one programmed host-buffer drain.
    drains: Mutex<std::collections::VecDeque<Result<Vec<JsObject>, HostError>>>,
    /// Leaves the input_accepted callback pending forever, so a received final
    /// stays unapplied and only the host's bounded cancel exit can surface it.
    block_input_accepted: bool,
    /// Leaves the model_event callback pending forever. Non-lifecycle events
    /// are applied too, so the application queue (final included) cannot grow
    /// past it; the final is received but can never be applied.
    block_model_event: bool,
    /// Leaves the turn_failed callback pending forever, so a received
    /// contradiction is never delivered and only the cancel exit can surface
    /// the success final that follows it.
    block_turn_failed: bool,
    /// Rejects input_accepted in validate_event: a legal lifecycle payload an
    /// inner handler refuses, to prove the StreamHandler wrapper forwards the
    /// inner contract's rejection instead of silently skipping it.
    reject_input_accepted: bool,
}

impl HostHandler for Handler {
    fn module_call(&self, _call: JsValue) -> HostFuture<JsObject> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Box::pin(async { Ok(JsObject::new()) })
    }

    fn event(&self, event: JsValue) -> HostFuture<()> {
        let payload_type = event
            .get("payload")
            .and_then(|payload| payload.get("type"))
            .cloned();
        let block = (self.block_input_accepted && payload_type == Some("input_accepted".into()))
            || (self.block_model_event && payload_type == Some("model_event".into()))
            || (self.block_turn_failed && payload_type == Some("turn_failed".into()));
        self.events.lock().unwrap().push(event);
        let fail = self.fail_event;
        Box::pin(async move {
            if fail {
                Err(HostError::new(HostErrorKind::Callback, "event failed"))
            } else if block {
                // Controllable pending callback (TurnScope shutdown cancels it).
                std::future::pending().await
            } else {
                Ok(())
            }
        })
    }

    fn validate_event(&self, payload: &JsValue) -> Result<EventGuard, HostError> {
        if self.reject_input_accepted && payload.get("type") == Some(&"input_accepted".into()) {
            return Err(HostError::new(
                HostErrorKind::Protocol,
                "inner rejected input_accepted",
            ));
        }
        Ok(EventGuard::default())
    }

    fn drain_events(&self) -> Result<Vec<JsObject>, HostError> {
        self.drains
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or(Ok(vec![]))
    }

    fn dispose(&self) -> HostFuture<()> {
        self.disposed.fetch_add(1, Ordering::SeqCst);
        Box::pin(async { Ok(()) })
    }
}

fn options() -> JsValue {
    parse_json(r#"{"sessionId":"session","turnId":"turn","runId":"run","operationId":"operation","requestId":"request"}"#).unwrap()
}

fn turn_options(turn: &str) -> JsValue {
    let mut fields = options().as_object().unwrap().clone();
    fields.insert("turnId".into(), turn.into());
    JsValue::Object(fields)
}

fn input() -> JsValue {
    parse_json(r#"{"type":"text","text":"original\ud800"}"#).unwrap()
}

fn input_messages() -> JsValue {
    parse_json(r#"[{"role":"user","content":[{"type":"text","text":"original\ud800"}]}]"#).unwrap()
}

fn wire(payload: &JsValue, final_event: bool) -> JsValue {
    let mut event = JsObject::new();
    event.insert("payload".into(), payload.clone());
    event.insert("final".into(), JsValue::Bool(final_event));
    JsValue::Object(event)
}

fn stream(
    execution: HostFuture<ExecuteResult>,
    events: mpsc::Receiver<JsValue>,
    integrity: Arc<StreamIntegrity>,
) -> LiveTurnStream {
    // Directly-built streams bypass the host receive path, so nothing has
    // been observed by the hook; finish() then rejects any Terminal unless
    // the test also drives the StreamHandler's validate_event in host order.
    LiveTurnStream {
        execution: Some(execution),
        result: None,
        events,
        identity: parse_execute_request(&input(), &options(), vec![])
            .unwrap()
            .0
            .identity,
        input: parse_execute_request(&input(), &options(), vec![])
            .unwrap()
            .1,
        integrity,
        delivered: StreamIntegrity::new(),
        active: None,
        recording_ready: oneshot::channel().1,
        recording: None,
        pending_event: None,
        accepted: false,
        preflight: 0,
        execute_ready: None,
        recorded: std::collections::VecDeque::new(),
    }
}

/// Simulate the host's receive order for one event: wire validation, the
/// synchronous lifecycle hook, then application (which publishes the payload
/// to the session consumer). Finals skip the hook, like the host does.
async fn deliver(
    handler: &StreamHandler,
    payload: &JsValue,
    final_event: bool,
) -> Result<(), HostError> {
    let _guard = if !final_event {
        handler.validate_event(payload)?
    } else {
        EventGuard::default()
    };
    handler.event(wire(payload, final_event)).await
}

async fn next(turn: &mut dyn LiveTurn) -> Result<Option<JsValue>, RuntimeError> {
    timeout(Duration::from_secs(15), turn.next())
        .await
        .expect("LiveTurn must make progress")
}

async fn persisted_failure(turn: &mut dyn LiveTurn) -> String {
    let mut message = None;
    for _ in 0..12 {
        let event = next(turn)
            .await
            .unwrap()
            .expect("failure terminal required");
        match event
            .get("type")
            .and_then(JsValue::as_string)
            .unwrap()
            .to_utf8()
            .unwrap()
            .as_str()
        {
            "turn_started" | "input_accepted" | "agent_status" => {}
            "turn_failed" => {
                message = event
                    .get("error")
                    .and_then(|e| e.get("message"))
                    .and_then(JsValue::as_string)
                    .and_then(|s| s.to_utf8().ok())
            }
            "turn_completed" => {
                assert_eq!(
                    event.get("result").unwrap().get("type"),
                    Some(&"error".into())
                );
                assert_eq!(
                    next(turn).await.unwrap(),
                    None,
                    "one persisted failure terminal"
                );
                return message.expect("host failure retains rejection message");
            }
            other => panic!("unexpected event before failure: {other}"),
        }
    }
    panic!("failure terminal not delivered")
}

fn event_types(events: &[JsValue]) -> Vec<String> {
    events
        .iter()
        .map(|event| {
            event
                .get("type")
                .and_then(JsValue::as_string)
                .expect("every delivered event carries a type")
                .to_utf8()
                .unwrap()
        })
        .collect()
}

/// Sync point for the slow-consumer tests: drive the turn until the host's
/// execution loop has read the module final into its application queue
/// (`final_received`), returning the events the consumer took on the way.
///
/// The execution future is only polled from `next()`. When the peer's remaining
/// frames were still in the pipe or the connection buffer, a consumer that
/// stalled first starved that receive path, so the post-cancel deadline fired
/// before the final was even queued and (by design) reported the accepted
/// timeout. Waiting for the received final removes that race; it does not
/// lengthen the grace and does not block a handler callback.
///
/// `biased` with the `next()` arm first keeps the wait cancellation-safe: a
/// `next()` that takes an event returns it in the same poll, so the future is
/// only dropped after reporting `Pending`, when it cannot have taken one.
async fn drive_until_final_received(
    turn: &mut dyn LiveTurn,
    final_received: &AtomicBool,
) -> Vec<JsValue> {
    let mut delivered = Vec::new();
    timeout(FINAL_SYNC_TIMEOUT, async {
        loop {
            tokio::select! {
                biased;
                event = turn.next() => match event {
                    Ok(Some(event)) => delivered.push(event),
                    Ok(None) => panic!("turn ended before the host received its final"),
                    Err(error) => {
                        panic!("turn failed before the host received its final: {error:?}")
                    }
                },
                _ = wait_for_final(final_received) => break,
            }
        }
    })
    .await
    .expect("the host must receive the module final before the consumer stalls");
    delivered
}

async fn wait_for_final(final_received: &AtomicBool) {
    // The sleep only paces the exact flag check; it can never make the
    // condition true early.
    while !final_received.load(Ordering::SeqCst) {
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
}

fn assert_aborted(event: &JsValue) {
    assert_aborted_turn(event, "turn");
}

fn assert_aborted_turn(event: &JsValue, turn: &str) {
    assert_eq!(event.get("type"), Some(&"turn_completed".into()));
    assert_eq!(event.get("sessionId"), Some(&"session".into()));
    assert_eq!(event.get("turnId"), Some(&turn.into()));
    assert_eq!(event.get("messages"), Some(&input_messages()));
    let result = event.get("result").unwrap();
    for (key, expected) in [
        ("type", "aborted".into()),
        ("sessionId", "session".into()),
        ("turnId", turn.into()),
        ("stopReason", "aborted_streaming".into()),
        ("turns", JsValue::Number(0.0)),
        ("usage", JsValue::Object(JsObject::new())),
        ("permissionDenials", JsValue::Array(vec![])),
    ] {
        assert_eq!(result.get(key), Some(&expected), "{key}");
    }
    let started = result
        .get("startedAt")
        .and_then(JsValue::as_string)
        .unwrap();
    assert!(started.to_utf8().unwrap().ends_with('Z'));
    assert_eq!(result.get("startedAt"), result.get("completedAt"));
    let projected = GatewayAgentEventProjector::default()
        .project(&GatewayAgentEventProjectionInput {
            event,
            run_id: "run",
            forward_subagent_text: false,
        })
        .unwrap();
    assert_eq!(projected.len(), 1);
    assert_eq!(projected[0].get("type"), Some(&"turn_completed".into()));
    assert_eq!(
        projected[0].get("finishReason"),
        Some(&"aborted_streaming".into())
    );
}

#[tokio::test]
async fn next_streams_payloads_with_backpressure_and_projects_final_once() {
    let expected = [
        r#"{"type":"turn_started","sessionId":"session","turnId":"turn"}"#,
        r#"{"type":"input_accepted","sessionId":"session","turnId":"turn","inputId":"input"}"#,
        r#"{"type":"model_event","sessionId":"session","event":{"type":"text_delta","text":"hello"}}"#,
    ].map(|value| parse_json(value).unwrap());
    let terminal = parse_json(r#"{"result":{"type":"success","sessionId":"session","turnId":"turn","stopReason":"completed","turns":1},"messages":[]}"#).unwrap();
    let (tx, rx) = mpsc::channel(1);
    let inner = Arc::new(Handler::default());
    let handler = Arc::new(StreamHandler {
        inner: inner.clone(),
        events: tx,
        integrity: Arc::new(StreamIntegrity::new()),
        forwarding: Arc::default(),
    });
    let integrity = handler.integrity.clone();
    let sent = expected.clone();
    let final_terminal = terminal.clone();
    let mut turn = stream(
        Box::pin(async move {
            handler.module_call(JsValue::Null).await?;
            for event in &sent {
                deliver(&handler, event, false).await?;
            }
            deliver(&handler, &final_terminal, true).await?;
            handler.dispose().await?;
            Ok(ExecuteResult::Terminal(wire(&final_terminal, true)))
        }),
        rx,
        integrity,
    );
    for event in expected {
        assert_eq!(next(&mut turn).await.unwrap(), Some(event));
    }
    let completed = next(&mut turn).await.unwrap().unwrap();
    assert_eq!(completed.get("type"), Some(&"turn_completed".into()));
    assert_eq!(completed.get("result"), terminal.get("result"));
    assert_eq!(completed.get("messages"), terminal.get("messages"));
    assert_eq!(next(&mut turn).await.unwrap(), None);
    assert_eq!(next(&mut turn).await.unwrap(), None);
    assert_eq!(inner.events.lock().unwrap().len(), 4);
    assert_eq!(inner.calls.load(Ordering::SeqCst), 1);
    assert_eq!(inner.disposed.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn event_queued_in_the_completing_poll_precedes_execution_error() {
    let (tx, rx) = mpsc::channel(1);
    let event =
        parse_json(r#"{"type":"turn_started","sessionId":"session","turnId":"turn"}"#).unwrap();
    let sent = event.clone();
    let mut turn = stream(
        Box::pin(async move {
            tx.send(sent).await.unwrap();
            Err(HostError::new(HostErrorKind::Disconnected, "lost peer"))
        }),
        rx,
        Arc::new(StreamIntegrity::new()),
    );
    assert_eq!(next(&mut turn).await.unwrap(), Some(event));
    assert!(
        next(&mut turn)
            .await
            .unwrap_err()
            .message
            .contains("lost peer")
    );
    assert_eq!(next(&mut turn).await.unwrap(), None);
}

#[tokio::test]
async fn failed_handler_event_is_not_published() {
    let (tx, rx) = mpsc::channel(1);
    let handler = Arc::new(StreamHandler {
        inner: Arc::new(Handler {
            fail_event: true,
            ..Handler::default()
        }),
        events: tx,
        integrity: Arc::new(StreamIntegrity::new()),
        forwarding: Arc::default(),
    });
    let integrity = handler.integrity.clone();
    let mut turn = stream(
        Box::pin(async move {
            deliver(
                &handler,
                &parse_json(r#"{"type":"turn_started"}"#).unwrap(),
                false,
            )
            .await?;
            Ok(ExecuteResult::Aborted { terminal: None })
        }),
        rx,
        integrity,
    );
    assert!(
        next(&mut turn)
            .await
            .unwrap_err()
            .message
            .contains("event failed")
    );
    assert_eq!(next(&mut turn).await.unwrap(), None);
}

#[tokio::test]
async fn closed_event_channel_waits_for_execution_cleanup_and_aborted_result() {
    let (tx, rx) = mpsc::channel(1);
    drop(tx);
    let mut turn = stream(
        Box::pin(async {
            tokio::task::yield_now().await;
            Ok(ExecuteResult::Aborted { terminal: None })
        }),
        rx,
        Arc::new(StreamIntegrity::new()),
    );
    assert_aborted(&next(&mut turn).await.unwrap().unwrap());
    assert_eq!(next(&mut turn).await.unwrap(), None);
}

#[tokio::test]
async fn aborted_result_already_supplied_by_host_is_projected_once() {
    let (_tx, rx) = mpsc::channel(1);
    let terminal = parse_json(r#"{"result":{"type":"aborted","stopReason":"aborted_streaming","turns":0},"messages":[{"role":"user","content":"kept"}]}"#).unwrap();
    let expected = terminal.clone();
    let mut turn = stream(
        Box::pin(async move {
            Ok(ExecuteResult::Aborted {
                terminal: Some(terminal),
            })
        }),
        rx,
        Arc::new(StreamIntegrity::new()),
    );
    let event = next(&mut turn).await.unwrap().unwrap();
    assert_eq!(event.get("result"), expected.get("result"));
    assert_eq!(event.get("messages"), expected.get("messages"));
    assert_eq!(next(&mut turn).await.unwrap(), None);
}

#[tokio::test]
async fn event_after_the_authoritative_terminal_fails_closed() {
    // After the module final the host stops receiving (execution.rs guards
    // `receive()` with `!terminal_received`) and the channel is quiescent
    // once execute settles, so a frame published after the final cannot reach
    // the session through the production path. What must still hold is that
    // the receive hook rejects any such event as a premature final once the
    // session projected the terminal, failing the execution closed instead of
    // extending the turn.
    let (tx, rx) = mpsc::channel(1);
    let terminal = parse_json(
        r#"{"result":{"type":"success","sessionId":"session","turnId":"turn","stopReason":"completed","turns":1},"messages":[]}"#,
    )
    .unwrap();
    let started =
        parse_json(r#"{"type":"turn_started","sessionId":"session","turnId":"turn"}"#).unwrap();
    let accepted =
        parse_json(r#"{"type":"input_accepted","sessionId":"session","turnId":"turn"}"#).unwrap();
    let handler = Arc::new(StreamHandler {
        inner: Arc::new(Handler::default()),
        events: tx,
        integrity: Arc::new(StreamIntegrity::new()),
        forwarding: Arc::default(),
    });
    let integrity = handler.integrity.clone();
    let mut turn = stream(
        Box::pin({
            let handler = handler.clone();
            async move {
                deliver(&handler, &started, false).await?;
                deliver(&handler, &accepted, false).await?;
                deliver(&handler, &terminal, true).await?;
                Ok(ExecuteResult::Terminal(wire(&terminal, true)))
            }
        }),
        rx,
        integrity,
    );
    assert_eq!(
        next(&mut turn).await.unwrap().unwrap().get("type"),
        Some(&"turn_started".into())
    );
    assert_eq!(
        next(&mut turn).await.unwrap().unwrap().get("type"),
        Some(&"input_accepted".into())
    );
    let completed = next(&mut turn).await.unwrap().unwrap();
    assert_eq!(completed.get("type"), Some(&"turn_completed".into()));
    // The terminal has now been projected; the hook must fail a later frame
    // instead of extending the closed turn.
    let error = handler
        .validate_event(&parse_json(r#"{"type":"model_event"}"#).unwrap())
        .expect_err("a post-terminal event must fail the hook");
    assert!(error.message.contains("premature final"), "{error:?}");
    assert_eq!(next(&mut turn).await.unwrap(), None);
}

async fn spawn_client(mode: &str) -> Result<HostClient, HostError> {
    let mut command = Command::new("python3");
    if mode.starts_with("live-") {
        command
            .arg(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/tests/fixtures/live-session/peer.py"
            ))
            .arg(mode);
    } else {
        command
            .arg(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../scripts/a6-host/fixture.py"
            ))
            .arg(mode);
    }
    HostClient::spawn(
        &mut command,
        Some(&"fixture".into()),
        Limits {
            handshake_timeout: Duration::from_secs(5),
            execution_timeout: Some(Duration::from_secs(10)),
            ..Limits::default()
        },
    )
    .await
}

async fn session(mode: &str, handler: Arc<Handler>) -> LiveSessionAdapter {
    session_with_final(mode, handler).await.0
}

/// `session`, plus the host-side "the module final entered the application
/// queue" observation the slow-consumer tests synchronize on.
async fn session_with_final(
    mode: &str,
    handler: Arc<Handler>,
) -> (LiveSessionAdapter, Arc<AtomicBool>) {
    // Pre-connect to expose final_received before execution starts. Tests of
    // pre-cancel use lazy factories; later turns create their own connection.
    let client = spawn_client(mode).await.unwrap();
    let final_received = client.final_received();
    let initial = Mutex::new(Some(client));
    let mode = mode.to_owned();
    let session = LiveSessionAdapter::new(test_recorder(), Vec::new, move |_| {
        let initial = initial.lock().unwrap().take();
        let mode = mode.clone();
        // This test double only records lifecycle calls; it owns no modules.
        let handler = handler.clone();
        Box::pin(async move {
            let client = match initial {
                Some(client) => client,
                None => spawn_client(&mode).await?,
            };
            Ok((
                Box::pin(async move { Ok(client) }) as HostFuture<HostClient>,
                handler as Arc<dyn HostHandler>,
                test_lease(),
            ))
        })
    });
    (session, final_received)
}

/// A session whose spawned sidecar child PIDs are recorded, so tests can
/// observe that a failed-closed stream actually reaped its child process.
async fn session_tracking_children(
    mode: &str,
    handler: Arc<Handler>,
) -> (LiveSessionAdapter, Arc<Mutex<Vec<u32>>>) {
    let children = Arc::new(Mutex::new(Vec::new()));
    let tracked = children.clone();
    let mode = mode.to_owned();
    let adapter = LiveSessionAdapter::new(test_recorder(), Vec::new, move |_| {
        let tracked = tracked.clone();
        let mode = mode.clone();
        let handler = handler.clone();
        Box::pin(async move {
            let client = spawn_client(&mode).await?;
            if let Some(pid) = client.child_id() {
                tracked.lock().unwrap().push(pid);
            }
            Ok((
                Box::pin(async move { Ok(client) }) as HostFuture<HostClient>,
                handler as Arc<dyn HostHandler>,
                test_lease(),
            ))
        })
    });
    (adapter, children)
}

/// kill(pid, 0) style probe: succeeds while the child exists, SRCH once the
/// process is gone. Mirrors host::tests::assert_reaped.
async fn assert_reaped(pid: u32) {
    timeout(Duration::from_secs(5), async {
        loop {
            match rustix::process::test_kill_process(
                rustix::process::Pid::from_raw(pid as i32).unwrap(),
            ) {
                Err(rustix::io::Errno::SRCH) => break,
                Ok(()) => tokio::time::sleep(Duration::from_millis(10)).await,
                Err(error) => panic!("cannot inspect sidecar child {pid}: {error}"),
            }
        }
    })
    .await
    .expect("sidecar child was not closed after the stream failed closed");
}

#[tokio::test]
async fn host_normal_lifecycle_streams_events_and_projects_authoritative_final_once() {
    let handler = Arc::new(Handler::default());
    let session = session("live-normal", handler.clone()).await;
    let mut turn = session.submit(input(), options()).unwrap();
    let expected = [
        "turn_started",
        "input_accepted",
        "model_event",
        "agent_status",
        "tool_progress",
    ];
    for event_type in expected {
        let event = next(turn.as_mut()).await.unwrap().unwrap();
        assert_eq!(event.get("type"), Some(&event_type.into()), "{event_type}");
    }
    let completed = next(turn.as_mut()).await.unwrap().unwrap();
    assert_eq!(completed.get("type"), Some(&"turn_completed".into()));
    assert_eq!(completed.get("sessionId"), Some(&"session".into()));
    assert_eq!(completed.get("turnId"), Some(&"turn".into()));
    let result = completed.get("result").unwrap();
    assert_eq!(result.get("type"), Some(&"success".into()));
    assert_eq!(result.get("stopReason"), Some(&"completed".into()));
    assert_eq!(
        completed.get("messages"),
        Some(&input_messages()),
        "the final must retain the echoed turn messages"
    );
    assert_eq!(next(turn.as_mut()).await.unwrap(), None);
    assert_eq!(next(turn.as_mut()).await.unwrap(), None);
    // turn_started, input_accepted, three intermediates and the applied final;
    // the sidecar's own turn_completed never reaches the handler either: the
    // host suppresses it (host/execution.rs, mirroring
    // agentLoopSidecarClient.ts:680-684), so the session-level rejection of a
    // turn_completed is only reachable through direct construction.
    assert_eq!(handler.events.lock().unwrap().len(), 6);
    assert_eq!(handler.disposed.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn host_early_failure_completes_after_host_input_accepted() {
    let handler = Arc::new(Handler::default());
    let session = session("live-early-failure", handler.clone()).await;
    let mut turn = session.submit(input(), options()).unwrap();
    for event_type in [
        "turn_started",
        "input_accepted",
        "agent_status",
        "turn_failed",
    ] {
        let event = next(turn.as_mut()).await.unwrap().unwrap();
        assert_eq!(event.get("type"), Some(&event_type.into()), "{event_type}");
    }
    let completed = next(turn.as_mut()).await.unwrap().unwrap();
    assert_eq!(completed.get("type"), Some(&"turn_completed".into()));
    let result = completed.get("result").unwrap();
    assert_eq!(result.get("type"), Some(&"error".into()));
    assert_eq!(result.get("stopReason"), Some(&"error".into()));
    assert_eq!(
        completed.get("messages"),
        Some(&input_messages()),
        "the error terminal still retains the turn messages"
    );
    assert_eq!(next(turn.as_mut()).await.unwrap(), None);
    assert_eq!(handler.events.lock().unwrap().len(), 4);
    assert_eq!(handler.disposed.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn host_second_turn_failed_from_finalization_failure_is_accepted() {
    // TurnRunner.ts:397-402 forwards the loop's turn_failed; a finalization
    // step (:408-419) may then throw and the catch (:437) yields a second
    // turn_failed before the same error terminal (:439). Rejecting the second
    // one would drop a legal TS error-final stream.
    let handler = Arc::new(Handler::default());
    let session = session("live-double-failure", handler.clone()).await;
    let mut turn = session.submit(input(), options()).unwrap();
    for event_type in [
        "turn_started",
        "input_accepted",
        "turn_failed",
        "turn_failed",
    ] {
        let event = next(turn.as_mut()).await.unwrap().unwrap();
        assert_eq!(event.get("type"), Some(&event_type.into()), "{event_type}");
    }
    let completed = next(turn.as_mut()).await.unwrap().unwrap();
    assert_eq!(completed.get("type"), Some(&"turn_completed".into()));
    let result = completed.get("result").unwrap();
    assert_eq!(result.get("type"), Some(&"error".into()));
    assert_eq!(result.get("stopReason"), Some(&"model_error".into()));
    assert_eq!(
        completed.get("messages"),
        Some(&input_messages()),
        "the error terminal still retains the turn messages"
    );
    assert_eq!(next(turn.as_mut()).await.unwrap(), None);
    assert_eq!(handler.events.lock().unwrap().len(), 5);
    assert_eq!(handler.disposed.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn host_max_turns_terminal_completes_without_turn_failed() {
    // AgentLoop.ts:2210-2233: max_turns yields a status event and the module
    // final only; no turn_failed may be required on this legal terminal.
    let handler = Arc::new(Handler::default());
    let session = session("live-max-turns", handler.clone()).await;
    let mut turn = session.submit(input(), options()).unwrap();
    for event_type in ["turn_started", "input_accepted", "agent_status"] {
        let event = next(turn.as_mut()).await.unwrap().unwrap();
        assert_eq!(event.get("type"), Some(&event_type.into()), "{event_type}");
    }
    let completed = next(turn.as_mut()).await.unwrap().unwrap();
    assert_eq!(completed.get("type"), Some(&"turn_completed".into()));
    let result = completed.get("result").unwrap();
    assert_eq!(result.get("type"), Some(&"max_turns".into()));
    assert_eq!(result.get("stopReason"), Some(&"max_turns".into()));
    assert_eq!(next(turn.as_mut()).await.unwrap(), None);
    assert_eq!(handler.events.lock().unwrap().len(), 4);
    assert_eq!(handler.disposed.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn inner_validation_rejection_fails_the_stream_before_application() {
    // The wrapper must keep the inner handler's validate_event semantics: an
    // inner handler that rejects a legal lifecycle payload (a module-side
    // contract the same handler would enforce when given to HostClient
    // directly) must also reject it through the LiveSession path — the event
    // is not applied, not published, and the turn fails closed with the
    // inner error.
    let handler = Arc::new(Handler {
        reject_input_accepted: true,
        ..Handler::default()
    });
    let session = session("live-normal", handler.clone()).await;
    let mut turn = session.submit(input(), options()).unwrap();
    let error = persisted_failure(turn.as_mut()).await;
    assert!(error.contains("inner rejected input_accepted"), "{error}");
    // input_accepted was never applied to the module-side handler...
    {
        let applied = handler.events.lock().unwrap();
        assert!(
            !applied.iter().any(|event| {
                event.get("payload").and_then(|payload| payload.get("type"))
                    == Some(&"input_accepted".into())
            }),
            "the inner-rejected event must not be applied"
        );
    }
    // ...and the failed-closed turn still disposed its host exactly once.
    assert_eq!(handler.disposed.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn cancel_exit_success_without_delivered_input_accepted_is_rejected() {
    // The pending input_accepted callback keeps the received success final
    // unapplied; after cancellation the host's bounded cancel exit
    // (POST_CANCEL_EXIT_TIMEOUT, host/execution.rs) stops applying events and
    // returns that already-validated final anyway. The consumer never received
    // input_accepted, so it must not be told the turn succeeded: TS reads the
    // final only after yielding every earlier event
    // (agentLoopSidecarClient.ts:552-683), and submit.rs only commits accepted
    // input when it sees input_accepted. Fail closed instead of accepting on
    // the receive-path fact.
    let handler = Arc::new(Handler {
        block_input_accepted: true,
        ..Handler::default()
    });
    let (session, final_received) =
        session_with_final("live-cancel-pending", handler.clone()).await;
    let mut turn = session.submit(input(), options()).unwrap();
    assert_eq!(
        next(turn.as_mut()).await.unwrap().unwrap().get("type"),
        Some(&"turn_started".into())
    );
    drive_until_final_received(turn.as_mut(), &final_received).await;
    session.abort(&"user-stop".into()).unwrap();
    let error = persisted_failure(turn.as_mut()).await;
    assert!(
        error.contains("terminal without proper lifecycle"),
        "{error:?}"
    );
    assert_eq!(next(turn.as_mut()).await.unwrap(), None);
    assert_eq!(handler.disposed.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn queued_turn_failed_dropped_by_the_cancel_exit_is_rejected() {
    // The pending input_accepted callback also holds back turn_failed and the
    // error final. The hook observed them at receive time, but the consumer
    // never did, so the error terminal would reach submit without its
    // turn_failed and the failure code would degrade. TS only publishes the
    // final after yielding turn_failed; fail closed.
    let handler = Arc::new(Handler {
        block_input_accepted: true,
        ..Handler::default()
    });
    let (session, final_received) =
        session_with_final("live-cancel-pending-failure", handler.clone()).await;
    let mut turn = session.submit(input(), options()).unwrap();
    assert_eq!(
        next(turn.as_mut()).await.unwrap().unwrap().get("type"),
        Some(&"turn_started".into())
    );
    drive_until_final_received(turn.as_mut(), &final_received).await;
    session.abort(&"user-stop".into()).unwrap();
    let error = persisted_failure(turn.as_mut()).await;
    assert!(
        error.contains("terminal without proper lifecycle"),
        "{error:?}"
    );
    assert_eq!(next(turn.as_mut()).await.unwrap(), None);
    assert_eq!(handler.disposed.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn cancel_exit_success_without_input_accepted_rejects_the_relaxed_path() {
    // P2 negative control: this test really reaches the host's cancel exit
    // (host/execution.rs:296-302), unlike the three lifecycle negative
    // controls whose finals are applied normally. turn_started is delivered;
    // a non-lifecycle model_event blocks the application callback forever, so
    // the success final is received into known_terminal but can never be
    // applied, and input_accepted is never sent at all. When the abort's
    // POST_CANCEL_EXIT_TIMEOUT expires the host returns that received final.
    // The session must reject it: a future relaxation of the form "a terminal
    // returned by the cancel exit skips the lifecycle check" must not turn
    // this green.
    let handler = Arc::new(Handler {
        block_model_event: true,
        ..Handler::default()
    });
    let (session, final_received) =
        session_with_final("live-cancel-blocked-model-event", handler.clone()).await;
    let mut turn = session.submit(input(), options()).unwrap();
    assert_eq!(
        next(turn.as_mut()).await.unwrap().unwrap().get("type"),
        Some(&"turn_started".into())
    );
    drive_until_final_received(turn.as_mut(), &final_received).await;
    session.abort(&"user-stop".into()).unwrap();
    let error = persisted_failure(turn.as_mut()).await;
    assert!(
        error.contains("terminal without proper lifecycle"),
        "{error:?}"
    );
    assert_eq!(next(turn.as_mut()).await.unwrap(), None);
    // The final never reached the module-side handler, so the rejected terminal
    // came from the cancel-exit branch, not from normal application. The
    // blocked model_event entered its application callback and stayed parked
    // there; it did not complete.
    let applied = handler.events.lock().unwrap();
    assert_eq!(
        applied.len(),
        2,
        "turn_started completed; the blocked model_event entered its callback; no final"
    );
    assert_eq!(
        applied[1]
            .get("payload")
            .and_then(|payload| payload.get("type")),
        Some(&"model_event".into())
    );
    assert_eq!(handler.disposed.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn received_turn_failed_dropped_after_delivered_input_accepted_is_rejected() {
    // Isolated negative control for finish()'s second gate
    // (StreamIntegrity::observe_terminal on the receive-path instance): the
    // consumer is delivered turn_started then input_accepted, but the received
    // turn_failed never completes its application and is dropped by the
    // bounded cancel exit together with the queued success final. The delivered
    // prefix alone would accept success (it ends at InputAccepted); only the
    // receive-path contradiction rejects it. Every other rejection test is
    // already caught by the delivered prefix, so deleting that second gate must
    // turn this test red while the rest of the suite stays green.
    let handler = Arc::new(Handler {
        block_turn_failed: true,
        ..Handler::default()
    });
    let (session, final_received) =
        session_with_final("live-cancel-queued-turn-failed", handler.clone()).await;
    let mut turn = session.submit(input(), options()).unwrap();
    for event_type in ["turn_started", "input_accepted"] {
        assert_eq!(
            next(turn.as_mut()).await.unwrap().unwrap().get("type"),
            Some(&event_type.into()),
            "{event_type}"
        );
    }
    drive_until_final_received(turn.as_mut(), &final_received).await;
    session.abort(&"user-stop".into()).unwrap();
    let error = persisted_failure(turn.as_mut()).await;
    assert!(
        error.contains("terminal without proper lifecycle"),
        "{error:?}"
    );
    assert_eq!(next(turn.as_mut()).await.unwrap(), None);
    // The blocked application proves the success final was never applied: the
    // rejected terminal came from the cancel-exit branch, not from normal
    // application.
    let applied = handler.events.lock().unwrap();
    let types: Vec<_> = applied
        .iter()
        .map(|event| {
            event
                .get("payload")
                .and_then(|payload| payload.get("type"))
                .cloned()
        })
        .collect();
    assert_eq!(
        types,
        vec![
            Some("turn_started".into()),
            Some("input_accepted".into()),
            Some("turn_failed".into())
        ],
        "turn_failed entered its callback and stayed parked; the final never ran"
    );
    assert_eq!(handler.disposed.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn slow_consumer_does_not_spend_the_cancel_exit_grace() {
    // P2 (opus concurrency): the host's cancel deadline is wall-clock, but its
    // execution future is only polled from next(). A consumer that spends
    // longer than POST_CANCEL_EXIT_TIMEOUT handling a delivered event must not
    // cause already-queued legal lifecycle events to be dropped: TS has no
    // post-cancel timer (agentLoopSidecarClient.ts:931-958; the accepted-diffs
    // entry at specs/gateway-parity/accepted-diffs.md:876 records the same) and
    // yields the final only after every earlier event, so this turn projects
    // success rather than a lifecycle error. No handler callback is blocked;
    // the stall is purely the consumer's.
    let handler = Arc::new(Handler::default());
    let (session, final_received) =
        session_with_final("live-cancel-slow-consumer", handler.clone()).await;
    let mut turn = session.submit(input(), options()).unwrap();
    let mut delivered = vec![next(turn.as_mut()).await.unwrap().unwrap()];
    assert_eq!(delivered[0].get("type"), Some(&"turn_started".into()));
    delivered.push(next(turn.as_mut()).await.unwrap().unwrap());
    assert_eq!(delivered[1].get("type"), Some(&"input_accepted".into()));
    // This next() is the first poll that observes the abort, so it arms the
    // cancel deadline; it delivers model_event and then the consumer stalls.
    delivered.push(next(turn.as_mut()).await.unwrap().unwrap());
    assert_eq!(delivered[2].get("type"), Some(&"model_event".into()));
    // Sync point before the stall: the peer has written every frame and the
    // host has read the success final into its application queue. Without it,
    // a slow CI peer could still be in the pipe, the unpolled receive path
    // would starve, and the deadline would correctly report a timeout.
    session.abort(&"user-stop".into()).unwrap();
    let synced = drive_until_final_received(turn.as_mut(), &final_received).await;
    assert!(
        !event_types(&synced).contains(&"turn_completed".to_owned()),
        "the consumer must stall before the terminal is delivered: {:?}",
        event_types(&synced)
    );
    delivered.extend(synced);
    // Do not hard-code the grace: one second past the host's own constant.
    tokio::time::sleep(POST_CANCEL_EXIT_TIMEOUT + Duration::from_secs(1)).await;
    while let Some(event) = next(turn.as_mut()).await.unwrap() {
        delivered.push(event);
    }
    // The queued input_accepted and success final must still be delivered, in
    // order. The sync point may move input_accepted into the pre-stall phase,
    // but it may not drop it or replace the success terminal with a timeout.
    assert_eq!(
        event_types(&delivered),
        [
            "turn_started",
            "input_accepted",
            "model_event",
            "turn_completed"
        ]
    );
    assert_eq!(
        delivered
            .last()
            .and_then(|event| event.get("result"))
            .and_then(|result| result.get("type")),
        Some(&"success".into())
    );
    assert_eq!(handler.disposed.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn slow_consumer_still_receives_a_ready_error_terminal() {
    // Pair for slow_consumer_does_not_spend_the_cancel_exit_grace: the same
    // stalled consumer, but the final is an error. turn_failed and the error
    // final are already queued when the grace expires; both must be delivered
    // in order, so the terminal is justified by a delivered turn_failed instead
    // of being rejected as "terminal without proper lifecycle". No handler
    // callback is blocked; the stall is purely the consumer's.
    let handler = Arc::new(Handler::default());
    let (session, final_received) =
        session_with_final("live-cancel-slow-consumer-failure", handler.clone()).await;
    let mut turn = session.submit(input(), options()).unwrap();
    let mut delivered = vec![next(turn.as_mut()).await.unwrap().unwrap()];
    assert_eq!(delivered[0].get("type"), Some(&"turn_started".into()));
    delivered.push(next(turn.as_mut()).await.unwrap().unwrap());
    assert_eq!(delivered[1].get("type"), Some(&"input_accepted".into()));
    // As in the success pairing, this next() arms the cancel deadline and
    // delivers model_event before the consumer stalls.
    delivered.push(next(turn.as_mut()).await.unwrap().unwrap());
    assert_eq!(delivered[2].get("type"), Some(&"model_event".into()));
    // Sync point before the stall: the turn_failed and the error final are in
    // the host's application queue before the consumer stops pulling.
    session.abort(&"user-stop".into()).unwrap();
    let synced = drive_until_final_received(turn.as_mut(), &final_received).await;
    assert!(
        !event_types(&synced).contains(&"turn_completed".to_owned()),
        "the consumer must stall before the terminal is delivered: {:?}",
        event_types(&synced)
    );
    delivered.extend(synced);
    tokio::time::sleep(POST_CANCEL_EXIT_TIMEOUT + Duration::from_secs(1)).await;
    while let Some(event) = next(turn.as_mut()).await.unwrap() {
        delivered.push(event);
    }
    // The queued turn_failed and error final must still be delivered in order.
    assert_eq!(
        event_types(&delivered),
        [
            "turn_started",
            "input_accepted",
            "model_event",
            "turn_failed",
            "turn_completed"
        ]
    );
    let completed = delivered.last().unwrap();
    assert_eq!(
        completed
            .get("result")
            .and_then(|result| result.get("type")),
        Some(&"error".into())
    );
    assert_eq!(
        completed
            .get("result")
            .and_then(|result| result.get("stopReason")),
        Some(&"model_error".into())
    );
    assert_eq!(handler.disposed.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn slow_consumer_with_a_blocked_callback_still_exits_bounded() {
    // The slow consumer lets the post-cancel deadline expire while the pending
    // input_accepted application is genuinely blocked (Handler::block_*): no
    // catch-up scheduling turn can finish it. The bounded exit must surface the
    // received final, fail the lifecycle check, and dispose exactly once
    // without waiting for the callback.
    let handler = Arc::new(Handler {
        block_input_accepted: true,
        ..Handler::default()
    });
    let (session, final_received) =
        session_with_final("live-cancel-slow-consumer", handler.clone()).await;
    let mut turn = session.submit(input(), options()).unwrap();
    assert_eq!(
        next(turn.as_mut()).await.unwrap().unwrap().get("type"),
        Some(&"turn_started".into())
    );
    assert_eq!(
        next(turn.as_mut()).await.unwrap().unwrap().get("type"),
        Some(&"input_accepted".into())
    );
    assert_eq!(
        next(turn.as_mut()).await.unwrap().unwrap().get("type"),
        Some(&"model_event".into())
    );
    // Sync point before the stall: the success final is in the host's
    // application queue even though the blocked callback keeps it unapplied.
    // The blocked input_accepted is not deliverable, so nothing is pulled.
    session.abort(&"user-stop".into()).unwrap();
    let synced = drive_until_final_received(turn.as_mut(), &final_received).await;
    assert!(
        synced.is_empty(),
        "a blocked callback must not deliver the queued lifecycle: {:?}",
        event_types(&synced)
    );
    tokio::time::sleep(POST_CANCEL_EXIT_TIMEOUT + Duration::from_secs(1)).await;
    // The deadline is already past; a blocked callback must not extend it.
    // Half a fresh POST_CANCEL_EXIT_TIMEOUT is enough to prove the exit was
    // not re-armed.
    let error = timeout(
        POST_CANCEL_EXIT_TIMEOUT / 2,
        persisted_failure(turn.as_mut()),
    )
    .await
    .expect("the cancel exit must stay bounded with a blocked callback");
    assert!(
        error.contains("terminal without proper lifecycle"),
        "{error:?}"
    );
    assert_eq!(next(turn.as_mut()).await.unwrap(), None);
    assert_eq!(handler.disposed.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn cancel_request_with_truly_missing_input_accepted_is_still_rejected() {
    // Negative control (a): the peer never sends input_accepted, so the
    // success final is missing a required lifecycle event no matter that a
    // cancellation request was made. A cancellation flag must not rescue it.
    // The round-1 implementation accepted this final (fake pass); the strict
    // delivered-prefix check rejects it (and the receive-path gate would too).
    let handler = Arc::new(Handler::default());
    let (session, final_received) =
        session_with_final("live-missing-accepted", handler.clone()).await;
    let mut turn = session.submit(input(), options()).unwrap();
    assert_eq!(
        next(turn.as_mut()).await.unwrap().unwrap().get("type"),
        Some(&"turn_started".into())
    );
    drive_until_final_received(turn.as_mut(), &final_received).await;
    session.abort(&"user-stop".into()).unwrap();
    let error = persisted_failure(turn.as_mut()).await;
    assert!(
        error.contains("terminal without proper lifecycle"),
        "{error:?}"
    );
    assert_eq!(next(turn.as_mut()).await.unwrap(), None);
    assert_eq!(handler.disposed.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn missing_lifecycle_is_rejected_without_cancellation() {
    // Negative control (c) through the production path: the same peer without
    // any cancellation request.
    let handler = Arc::new(Handler::default());
    let session = session("live-missing-accepted", handler.clone()).await;
    let mut turn = session.submit(input(), options()).unwrap();
    assert_eq!(
        next(turn.as_mut()).await.unwrap().unwrap().get("type"),
        Some(&"turn_started".into())
    );
    let error = persisted_failure(turn.as_mut()).await;
    assert!(
        error.contains("terminal without proper lifecycle"),
        "{error:?}"
    );
    assert_eq!(next(turn.as_mut()).await.unwrap(), None);
    assert_eq!(handler.disposed.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn success_after_observed_turn_failed_is_rejected_even_with_cancel() {
    // Negative control (b): after turn_failed was received, only the same
    // error terminal may follow (TurnRunner.ts:437-439); a success final is a
    // contradiction. The round-1 implementation accepted it under a
    // cancellation flag (fake pass); the strict check rejects it.
    let handler = Arc::new(Handler::default());
    let session = session("live-failed-then-success", handler.clone()).await;
    let mut turn = session.submit(input(), options()).unwrap();
    for event_type in ["turn_started", "input_accepted", "turn_failed"] {
        assert_eq!(
            next(turn.as_mut()).await.unwrap().unwrap().get("type"),
            Some(&event_type.into()),
            "{event_type}"
        );
    }
    session.abort(&"user-stop".into()).unwrap();
    let error = persisted_failure(turn.as_mut()).await;
    assert!(
        error.contains("terminal without proper lifecycle"),
        "{error:?}"
    );
    assert_eq!(next(turn.as_mut()).await.unwrap(), None);
    assert_eq!(handler.disposed.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn max_turns_after_observed_turn_failed_is_rejected_even_with_cancel() {
    // Negative control (b), max_turns variant: the loop's max-turns path never
    // emits turn_failed (AgentLoop.ts:2218-2233), so a max_turns final after a
    // received turn_failed contradicts it, cancellation or not.
    let handler = Arc::new(Handler::default());
    let session = session("live-failed-then-max-turns", handler.clone()).await;
    let mut turn = session.submit(input(), options()).unwrap();
    for event_type in ["turn_started", "input_accepted", "turn_failed"] {
        assert_eq!(
            next(turn.as_mut()).await.unwrap().unwrap().get("type"),
            Some(&event_type.into()),
            "{event_type}"
        );
    }
    session.abort(&"user-stop".into()).unwrap();
    let error = persisted_failure(turn.as_mut()).await;
    assert!(
        error.contains("terminal without proper lifecycle"),
        "{error:?}"
    );
    assert_eq!(next(turn.as_mut()).await.unwrap(), None);
    assert_eq!(handler.disposed.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn events_after_a_violation_settle_to_none_even_when_buffered() {
    // A legal event already buffered when the violation fails the execution
    // must be delivered before the violation error, never after it: the
    // biased recv arm and the try_recv drain both run before result.take(),
    // and the channel is quiescent once execute settles. next() then settles
    // to None.
    let (tx, rx) = mpsc::channel(1);
    let handler = Arc::new(StreamHandler {
        inner: Arc::new(Handler::default()),
        events: tx,
        integrity: Arc::new(StreamIntegrity::new()),
        forwarding: Arc::default(),
    });
    let integrity = handler.integrity.clone();
    let mut turn = stream(
        Box::pin(async move {
            deliver(
                &handler,
                &parse_json(r#"{"type":"turn_started"}"#).unwrap(),
                false,
            )
            .await?;
            // The legal turn_started was applied and published; only then
            // does the violating second one fail the hook, exactly like
            // the host receive path. Events after it are never received.
            Err(handler
                .validate_event(&parse_json(r#"{"type":"turn_started"}"#).unwrap())
                .expect_err("second turn_started must violate"))
        }),
        rx,
        integrity,
    );
    assert_eq!(
        next(&mut turn).await.unwrap().unwrap().get("type"),
        Some(&"turn_started".into())
    );
    let error = next(&mut turn).await.unwrap_err();
    assert!(
        error
            .message
            .contains("turn_started after the turn already opened"),
        "{error:?}"
    );
    assert_eq!(next(&mut turn).await.unwrap(), None);
    assert_eq!(next(&mut turn).await.unwrap(), None);
}

#[tokio::test]
async fn a_final_with_unobserved_lifecycle_is_rejected() {
    // Unit-level stream check: a hand-constructed Terminal bypasses the host
    // receive path, so nothing was observed and the strict terminal check
    // fails. The real-host negative controls below prove the same boundary
    // through the production path.
    let (_tx, rx) = mpsc::channel(1);
    let terminal = parse_json(
        r#"{"result":{"type":"success","sessionId":"session","turnId":"turn","stopReason":"completed","turns":1},"messages":[]}"#,
    )
    .unwrap();
    let mut turn = stream(
        Box::pin(async move { Ok(ExecuteResult::Terminal(terminal)) }),
        rx,
        Arc::new(StreamIntegrity::new()),
    );
    let error = next(&mut turn).await.unwrap_err();
    assert!(
        error.message.contains("terminal without proper lifecycle"),
        "{error:?}"
    );
    assert_eq!(next(&mut turn).await.unwrap(), None);
}

#[tokio::test]
async fn host_stream_rejects_events_before_turn_started_and_releases_the_host() {
    let handler = Arc::new(Handler::default());
    let (session, children) = session_tracking_children("turn-completed", handler.clone()).await;
    let mut turn = session.submit(input(), options()).unwrap();
    // The adversarial peer streams text_delta events with no turn_started;
    // the first delivered event must fail the turn closed.
    let error = persisted_failure(turn.as_mut()).await;
    assert!(error.contains("before turn_started"), "{error:?}");
    assert_eq!(next(turn.as_mut()).await.unwrap(), None);
    assert_eq!(next(turn.as_mut()).await.unwrap(), None);
    // The violation is caught by the receive-path hook before application, so
    // the inner handler never applied (and the host never ACKed) the illegal
    // event, and the dropped execution's detached cleanup still disposed the
    // handler exactly once.
    timeout(Duration::from_secs(5), async {
        while handler.disposed.load(Ordering::SeqCst) == 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("dropping execute must dispose its handler");
    assert_eq!(handler.disposed.load(Ordering::SeqCst), 1);
    // The misbehaving sidecar process itself was closed, not just the stream.
    let child = *children.lock().unwrap().first().unwrap();
    assert_reaped(child).await;
    assert!(
        handler.events.lock().unwrap().is_empty(),
        "a violating event must not be applied to the module-side handler"
    );
    // The active turn slot was released with the violation.
    let replacement = session.submit(input(), options());
    assert!(replacement.is_ok());
    drop(replacement);
}

#[tokio::test]
async fn abort_before_execute_yields_original_messages_then_none() {
    let handler = Arc::new(Handler::default());
    let (session, children) =
        session_tracking_children("cancel-pre-execute", handler.clone()).await;
    let mut turn = session.submit(input(), options()).unwrap();
    session.abort(&"pre-cancel".into()).unwrap();
    assert_aborted(&next(turn.as_mut()).await.unwrap().unwrap());
    assert_eq!(next(turn.as_mut()).await.unwrap(), None);
    assert_eq!(next(turn.as_mut()).await.unwrap(), None);
    assert!(handler.events.lock().unwrap().is_empty());
    assert_eq!(handler.disposed.load(Ordering::SeqCst), 0);
    assert!(
        children.lock().unwrap().is_empty(),
        "no sidecar may be created"
    );
}

#[tokio::test]
async fn incomplete_compaction_final_yields_original_messages_then_none() {
    let handler = Arc::new(Handler::default());
    let session = session("live-cancel-compaction", handler.clone()).await;
    let mut turn = session.submit(input(), options()).unwrap();
    for event_type in ["turn_started", "input_accepted", "model_event"] {
        let event = next(turn.as_mut()).await.unwrap().unwrap();
        assert_eq!(event.get("type"), Some(&event_type.into()), "{event_type}");
    }
    session
        .abort(&"compaction_persistence_failed".into())
        .unwrap();
    assert_aborted(&next(turn.as_mut()).await.unwrap().unwrap());
    assert_eq!(next(turn.as_mut()).await.unwrap(), None);
    // The incomplete compaction final is aborted before the handler sees it.
    assert_eq!(handler.events.lock().unwrap().len(), 3);
    assert_eq!(handler.disposed.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn cancel_with_a_full_aborted_result_projects_the_peer_terminal_once() {
    // The other side of the compaction case: TS AgentLoop.ts:594-611 returns a
    // complete result.type="aborted" module final after cancel, which the host
    // surfaces as ExecuteResult::Terminal — the authoritative-terminal
    // branch, not the ExecuteResult::Aborted synthesis. The session must
    // project the peer's own result and messages exactly once, then close and
    // release the host.
    let handler = Arc::new(Handler::default());
    let (session, children) =
        session_tracking_children("live-cancel-full-aborted", handler.clone()).await;
    let mut turn = session.submit(input(), options()).unwrap();
    for event_type in ["turn_started", "input_accepted", "model_event"] {
        let event = next(turn.as_mut()).await.unwrap().unwrap();
        assert_eq!(event.get("type"), Some(&event_type.into()), "{event_type}");
    }
    session.abort(&"user-stop".into()).unwrap();
    let completed = next(turn.as_mut()).await.unwrap().unwrap();
    assert_eq!(completed.get("type"), Some(&"turn_completed".into()));
    assert_eq!(completed.get("sessionId"), Some(&"session".into()));
    assert_eq!(completed.get("turnId"), Some(&"turn".into()));
    let result = completed.get("result").unwrap();
    // The peer's own result, not the session-synthesized aborted result:
    // a real turns count and distinct timestamps (aborted_result uses
    // turns 0 and startedAt == completedAt).
    assert_eq!(result.get("type"), Some(&"aborted".into()));
    assert_eq!(result.get("stopReason"), Some(&"aborted_streaming".into()));
    assert_eq!(result.get("turns"), Some(&JsValue::Number(1.0)));
    assert_ne!(result.get("startedAt"), result.get("completedAt"));
    assert_eq!(
        completed.get("messages"),
        Some(&input_messages()),
        "the aborted terminal still retains the turn messages"
    );
    // The authoritative terminal is projected exactly once, then closed.
    assert_eq!(next(turn.as_mut()).await.unwrap(), None);
    assert_eq!(next(turn.as_mut()).await.unwrap(), None);
    // turn_started, input_accepted, model_event and the applied final.
    assert_eq!(handler.events.lock().unwrap().len(), 4);
    assert_eq!(handler.disposed.load(Ordering::SeqCst), 1);
    // The sidecar process itself was released, not just the stream.
    let child = *children.lock().unwrap().first().unwrap();
    assert_reaped(child).await;
}

#[tokio::test]
async fn host_duplicate_lifecycle_event_fails_the_stream_closed() {
    let handler = Arc::new(Handler::default());
    let (session, children) =
        session_tracking_children("live-duplicate-start", handler.clone()).await;
    let mut turn = session.submit(input(), options()).unwrap();
    let error = persisted_failure(turn.as_mut()).await;
    assert!(
        error.contains("turn_started after the turn already opened"),
        "{error}"
    );
    // The peer also streams a legal model_event after the violating one; it
    // is never even received once the hook failed, and nothing may surface
    // after the stream has failed closed.
    assert_eq!(next(turn.as_mut()).await.unwrap(), None);
    assert_eq!(next(turn.as_mut()).await.unwrap(), None);
    timeout(Duration::from_secs(5), async {
        while handler.disposed.load(Ordering::SeqCst) == 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("dropping execute must dispose its handler");
    assert_eq!(handler.disposed.load(Ordering::SeqCst), 1);
    // Dropping execute closes the peer's whole process group: the peer, which
    // would otherwise block on stdin, actually exits.
    let child = *children.lock().unwrap().first().unwrap();
    assert_reaped(child).await;
    // The slot is free again after the violation.
    assert!(session.submit(input(), options()).is_ok());
}

#[tokio::test]
async fn dropping_an_active_stream_disposes_its_host_handler() {
    let handler = Arc::new(Handler::default());
    let session = session("live-normal", handler.clone()).await;
    let mut turn = session.submit(input(), options()).unwrap();
    assert!(next(turn.as_mut()).await.unwrap().is_some());
    drop(turn);
    timeout(Duration::from_secs(5), async {
        while handler.disposed.load(Ordering::SeqCst) == 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("dropping execute must dispose its handler");
    assert_eq!(handler.disposed.load(Ordering::SeqCst), 1);
    let mut second = session.submit(input(), turn_options("second")).unwrap();
    assert!(next(second.as_mut()).await.unwrap().is_some());
}

#[test]
fn execute_request_preserves_host_options_and_original_input() {
    let raw =
        parse_json(r#"{"type":"text","text":"raw\ud800","messages":["forged"],"model":"forged"}"#)
            .unwrap();
    let mut options = options().as_object().unwrap().clone();
    options.insert("model".into(), "host-model".into());
    options.insert(
        "messages".into(),
        JsValue::Array(vec!["also forged".into()]),
    );
    let (request, prepared) =
        parse_execute_request(&raw, &JsValue::Object(options), vec![]).unwrap();
    assert_eq!(
        request.payload.get("model"),
        Some(&"host-model".into()),
        "host config wins"
    );
    assert_eq!(
        request.payload.get("messages"),
        Some(
            &parse_json(r#"[{"role":"user","content":[{"type":"text","text":"raw\ud800"}]}]"#)
                .unwrap()
        ),
        "raw input wins"
    );
    assert!(matches!(
        prepared.input,
        super::super::runner::AgentInput::Text(_)
    ));
    assert_eq!(request.identity.session_id, JsString::from("session"));
    assert_eq!(request.identity.turn_id, JsString::from("turn"));
    assert_eq!(request.identity.run_id, JsString::from("run"));
    assert_eq!(request.identity.operation_id, JsString::from("operation"));
    assert_eq!(request.identity.request_id, JsString::from("request"));
}

#[test]
fn execute_request_rejects_flattened_and_invalid_input() {
    for raw in [
        "null",
        "true",
        "1",
        "[]",
        r#"{"messages":[]}"#,
        r#"{"type":"text","text":null}"#,
        r#"{"type":"blocks","content":{}}"#,
    ] {
        assert!(
            parse_execute_request(&parse_json(raw).unwrap(), &options(), vec![]).is_err(),
            "reject {raw}"
        );
    }
    let invalid = parse_json(r#"{"sessionId":"s","turnId":"t","syntheticMessages":{}}"#).unwrap();
    assert!(
        parse_execute_request(&input(), &invalid, vec![]).is_err(),
        "reject synthetic shape"
    );
}

#[tokio::test]
async fn sequential_turns_create_fresh_hosts_and_send_each_payload() {
    let handlers = Arc::new(Mutex::new(Vec::new()));
    let created = handlers.clone();
    let session = LiveSessionAdapter::new(test_recorder(), Vec::new, move |_| {
        let created = created.clone();
        Box::pin(async move {
            let client = spawn_client("live-payload").await?;
            let handler = Arc::new(Handler::default());
            created.lock().unwrap().push(handler.clone());
            Ok((
                Box::pin(async move { Ok(client) }) as HostFuture<HostClient>,
                handler as Arc<dyn HostHandler>,
                test_lease(),
            ))
        })
    });
    for number in 1_u32..=2 {
        let turn_id = JsValue::String(format!("turn-{number}").into());
        let mut turn_options = options().as_object().unwrap().clone();
        turn_options.insert("turnId".into(), turn_id.clone());
        turn_options.insert("custom".into(), JsValue::Number(f64::from(number)));
        turn_options.insert(
            "model".into(),
            JsValue::String(format!("model-{number}").into()),
        );
        let mut payload = turn_options.clone();
        payload.insert("messages".into(), input_messages());
        let payload = JsValue::Object(payload);
        let mut turn = session
            .submit(input(), JsValue::Object(turn_options))
            .unwrap();
        assert!(session.submit(input(), options()).is_err());
        let started = next(turn.as_mut()).await.unwrap().unwrap();
        assert_eq!(started.get("type"), Some(&"turn_started".into()));
        let received = next(turn.as_mut()).await.unwrap().unwrap();
        assert_eq!(received.get("messages"), Some(&input_messages()));
        assert_eq!(received.get("turnId"), Some(&turn_id));
        let completed = next(turn.as_mut()).await.unwrap().unwrap();
        assert_eq!(completed.get("type"), Some(&"turn_completed".into()));
        let result = completed.get("result").unwrap();
        assert_eq!(result.get("type"), Some(&"success".into()));
        assert_eq!(result.get("turnId"), Some(&turn_id));
        assert_eq!(next(turn.as_mut()).await.unwrap(), None);
        let handlers = handlers.lock().unwrap();
        assert_eq!(
            handlers.last().unwrap().events.lock().unwrap()[1]
                .get("payload")
                .unwrap()
                .get("receivedPayload"),
            Some(&payload)
        );
        assert_eq!(handlers.len(), number as usize);
        assert_eq!(handlers.last().unwrap().disposed.load(Ordering::SeqCst), 1);
    }
}

#[tokio::test]
async fn invalid_submit_does_not_consume_the_session() {
    let session = session("live-normal", Arc::new(Handler::default())).await;
    assert!(session.submit(JsValue::Null, options()).is_err());
    assert!(session.submit(input(), JsValue::Null).is_err());
    let mut turn = session.submit(input(), options()).unwrap();
    assert!(next(turn.as_mut()).await.unwrap().is_some());
}

#[tokio::test]
async fn unpolled_drop_and_factory_failure_release_the_active_turn() {
    let calls = Arc::new(AtomicUsize::new(0));
    let created = calls.clone();
    let session = LiveSessionAdapter::new(test_recorder(), Vec::new, move |_| {
        created.fetch_add(1, Ordering::SeqCst);
        Box::pin(async { Err(HostError::new(HostErrorKind::Io, "spawn failed")) })
    });
    let turn = session.submit(input(), options()).unwrap();
    assert!(session.submit(input(), options()).is_err());
    drop(turn);
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    for expected_calls in 1..=2 {
        let mut turn = session.submit(input(), options()).unwrap();
        assert!(
            next(turn.as_mut())
                .await
                .unwrap_err()
                .message
                .contains("spawn failed")
        );
        assert_eq!(next(turn.as_mut()).await.unwrap(), None);
        assert_eq!(calls.load(Ordering::SeqCst), expected_calls);
    }
}

#[tokio::test]
async fn abort_is_per_turn_and_dispose_rejects_future_turns() {
    let handler = Arc::new(Handler::default());
    let session = session("live-cancel-compaction", handler.clone()).await;
    let mut first = session.submit(input(), options()).unwrap();
    for event_type in ["turn_started", "input_accepted", "model_event"] {
        assert_eq!(
            next(first.as_mut()).await.unwrap().unwrap().get("type"),
            Some(&event_type.into()),
            "{event_type}"
        );
    }
    session
        .abort(&"compaction_persistence_failed".into())
        .unwrap();
    assert_aborted(&next(first.as_mut()).await.unwrap().unwrap());
    assert_eq!(next(first.as_mut()).await.unwrap(), None);
    let mut second = session.submit(input(), turn_options("second")).unwrap();
    // A new turn must reach the sidecar instead of inheriting the old abort.
    for event_type in ["turn_started", "input_accepted", "model_event"] {
        assert_eq!(
            next(second.as_mut()).await.unwrap().unwrap().get("type"),
            Some(&event_type.into()),
            "{event_type}"
        );
    }
    session
        .abort(&"compaction_persistence_failed".into())
        .unwrap();
    session.dispose().await.unwrap();
    assert!(session.submit(input(), options()).is_err());
    assert_aborted_turn(&next(second.as_mut()).await.unwrap().unwrap(), "second");
    assert_eq!(next(second.as_mut()).await.unwrap(), None);
    assert_eq!(handler.disposed.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn dispose_cancels_pending_host_creation_and_rejects_future_submits() {
    let started = Arc::new(tokio::sync::Notify::new());
    let entered = started.clone();
    let session = LiveSessionAdapter::new(test_recorder(), Vec::new, move |_| {
        entered.notify_one();
        Box::pin(std::future::pending())
    });
    let mut turn = session.submit(input(), options()).unwrap();
    let mut pending = turn.next();
    timeout(Duration::from_secs(5), async {
        tokio::select! {
            result = &mut pending => panic!("host creation must still be pending: {result:?}"),
            _ = started.notified() => {}
        }
    })
    .await
    .expect("host factory must start");
    session.dispose().await.unwrap();
    assert_aborted(
        &timeout(Duration::from_secs(5), pending)
            .await
            .expect("dispose must interrupt host creation")
            .unwrap()
            .unwrap(),
    );
    assert_eq!(next(turn.as_mut()).await.unwrap(), None);
    assert!(session.submit(input(), options()).is_err());
}

fn host_event(kind: &str) -> JsValue {
    let mut event = JsObject::new();
    event.insert("type".into(), kind.into());
    JsValue::Object(event)
}

fn queue_host_events(inner: &Handler, events: &[JsValue]) {
    inner.drains.lock().unwrap().push_back(Ok(events
        .iter()
        .map(|event| event.as_object().unwrap().clone())
        .collect()));
}

fn buffered_handler(
    capacity: usize,
) -> (Arc<Handler>, Arc<StreamHandler>, mpsc::Receiver<JsValue>) {
    let inner = Arc::new(Handler::default());
    let (events, receiver) = mpsc::channel(capacity);
    let handler = Arc::new(StreamHandler {
        inner: inner.clone(),
        events,
        integrity: Arc::new(StreamIntegrity::new()),
        forwarding: Arc::default(),
    });
    (inner, handler, receiver)
}

#[tokio::test]
async fn host_buffer_failures_cut_off_later_events_and_terminals() {
    for (bad_event, expected) in [
        (None, "host drain failed"),
        (
            Some("turn_started"),
            "turn_started after the turn already opened",
        ),
        (Some("input_accepted"), "input_accepted out of order"),
        (
            Some("turn_completed"),
            "sidecar turn_completed must not reach",
        ),
    ] {
        for aborted in [false, true] {
            let (inner, handler, rx) = buffered_handler(4);
            let integrity = handler.integrity.clone();
            let mut turn = stream(
                Box::pin(async move {
                    deliver(&handler, &host_event("turn_started"), false).await?;
                    deliver(&handler, &host_event("input_accepted"), false).await?;
                    if let Some(kind) = bad_event {
                        queue_host_events(&inner, &[host_event(kind)]);
                    } else {
                        inner.drains.lock().unwrap().push_back(Err(HostError::new(
                            HostErrorKind::Callback,
                            "host drain failed",
                        )));
                    }
                    let failed = handler.module_call(JsValue::Null);
                    if aborted {
                        // Cancellation may drop this future; synchronous capture
                        // must retain the failure independently of polling it.
                        drop(failed);
                    } else {
                        assert!(failed.await.unwrap_err().message.contains(expected));
                    }
                    // HostClient turns module Err into a response and keeps reading.
                    // Even a later otherwise-legal wire payload must be cut off.
                    let error = deliver(&handler, &host_event("model_event"), false)
                        .await
                        .expect_err("host error must cut off later wire events");
                    assert!(error.message.contains(expected), "{error:?}");
                    assert_eq!(inner.events.lock().unwrap().len(), 2);
                    queue_host_events(&inner, &[host_event("model_event")]);
                    assert!(
                        handler
                            .module_call(JsValue::Null)
                            .await
                            .unwrap_err()
                            .message
                            .contains(expected)
                    );
                    if aborted {
                        Ok(ExecuteResult::Aborted { terminal: None })
                    } else {
                        let terminal =
                            parse_json(r#"{"result":{"type":"success"},"messages":[]}"#).unwrap();
                        Ok(ExecuteResult::Terminal(wire(&terminal, true)))
                    }
                }),
                rx,
                integrity,
            );
            assert_eq!(
                next(&mut turn).await.unwrap(),
                Some(host_event("turn_started"))
            );
            assert_eq!(
                next(&mut turn).await.unwrap(),
                Some(host_event("input_accepted"))
            );
            let error = next(&mut turn).await.unwrap_err();
            assert!(error.message.contains(expected), "{error:?}");
            assert_eq!(next(&mut turn).await.unwrap(), None);
            assert_eq!(next(&mut turn).await.unwrap(), None);
        }
    }
}

#[tokio::test]
async fn host_buffer_before_turn_started_fails_closed() {
    for kind in ["model_event", "input_accepted"] {
        let (inner, handler, rx) = buffered_handler(4);
        let integrity = handler.integrity.clone();
        let mut turn = stream(
            Box::pin(async move {
                queue_host_events(&inner, &[host_event(kind)]);
                let error = handler.module_call(JsValue::Null).await.unwrap_err();
                assert!(error.message.contains("Stream integrity violation"));
                assert_eq!(
                    deliver(&handler, &host_event("turn_started"), false)
                        .await
                        .unwrap_err()
                        .message,
                    error.message
                );
                assert!(inner.events.lock().unwrap().is_empty());
                Ok(ExecuteResult::Aborted { terminal: None })
            }),
            rx,
            integrity,
        );
        assert!(
            next(&mut turn)
                .await
                .unwrap_err()
                .message
                .contains("Stream integrity violation")
        );
        assert_eq!(next(&mut turn).await.unwrap(), None);
    }
}

/// Poll once without sleeps or scheduler races, retaining the pending future.
async fn assert_pending<T>(future: &mut HostFuture<T>) {
    std::future::poll_fn(|cx| {
        assert!(future.as_mut().poll(cx).is_pending());
        std::task::Poll::Ready(())
    })
    .await;
}

#[tokio::test]
async fn host_buffer_batches_wait_for_reserved_wire_order() {
    let (inner, handler, mut rx) = buffered_handler(8);
    let started = host_event("turn_started");
    let accepted = host_event("input_accepted");
    let first = [host_event("module_first_a"), host_event("module_first_b")];
    let second = [host_event("module_second_a"), host_event("module_second_b")];
    let _started_guard = handler.validate_event(&started).unwrap();
    queue_host_events(&inner, &first);
    let mut first_call = handler.module_call(JsValue::Null);
    let _accepted_guard = handler.validate_event(&accepted).unwrap();
    queue_host_events(&inner, &second);
    let mut second_call = handler.module_call(JsValue::Null);
    // Reverse poll order cannot move either batch past the reserved wire slot.
    assert_pending(&mut second_call).await;
    assert_pending(&mut first_call).await;
    assert!(matches!(
        rx.try_recv(),
        Err(mpsc::error::TryRecvError::Empty)
    ));
    timeout(FINAL_SYNC_TIMEOUT, handler.event(wire(&started, false)))
        .await
        .unwrap()
        .unwrap();
    let mut accepted_call = handler.event(wire(&accepted, false));
    assert_pending(&mut accepted_call).await;
    timeout(FINAL_SYNC_TIMEOUT, first_call)
        .await
        .unwrap()
        .unwrap();
    assert_pending(&mut second_call).await;
    timeout(FINAL_SYNC_TIMEOUT, accepted_call)
        .await
        .unwrap()
        .unwrap();
    timeout(FINAL_SYNC_TIMEOUT, second_call)
        .await
        .unwrap()
        .unwrap();
    let expected = [
        vec![started],
        first.to_vec(),
        vec![accepted],
        second.to_vec(),
    ]
    .concat();
    for event in expected {
        assert_eq!(rx.try_recv().unwrap(), event);
    }
    assert!(matches!(
        rx.try_recv(),
        Err(mpsc::error::TryRecvError::Empty)
    ));
    handler
        .integrity
        .observe_terminal(Some(&"success".into()))
        .unwrap();
}

#[tokio::test]
async fn host_error_cuts_off_prevalidated_and_backpressured_deliveries() {
    for pending_send in [false, true] {
        let (inner, handler, mut rx) = buffered_handler(1);
        let started = host_event("turn_started");
        deliver(&handler, &started, false).await.unwrap(); // fill the channel
        let accepted = host_event("input_accepted");
        let _accepted_guard = handler.validate_event(&accepted).unwrap();
        let mut pending = if pending_send {
            let mut future = handler.event(wire(&accepted, false));
            assert_pending(&mut future).await; // blocked on capacity
            Some(future)
        } else {
            None
        };
        inner.drains.lock().unwrap().push_back(Err(HostError::new(
            HostErrorKind::Callback,
            "host drain failed while wire pending",
        )));
        assert!(
            timeout(FINAL_SYNC_TIMEOUT, handler.module_call(JsValue::Null))
                .await
                .unwrap()
                .is_err()
        );
        assert_eq!(rx.try_recv().unwrap(), started); // earlier applied prefix survives
        let future = pending
            .take()
            .unwrap_or_else(|| handler.event(wire(&accepted, false)));
        assert!(
            timeout(FINAL_SYNC_TIMEOUT, future)
                .await
                .unwrap()
                .expect_err("queued event must not survive host failure")
                .message
                .contains("host drain failed while wire pending")
        );
        assert!(matches!(
            rx.try_recv(),
            Err(mpsc::error::TryRecvError::Empty)
        ));
        assert!(handler.integrity.observe_aborted().is_err());
    }
}

#[tokio::test]
async fn non_final_event_missing_payload_is_protocol_error() {
    for raw in ["{}", r#"{"final":false}"#] {
        assert_invalid_event_payload(parse_json(raw).unwrap()).await;
    }
}

async fn assert_invalid_event_payload(event: JsValue) {
    let (inner, handler, mut receiver) = buffered_handler(4);
    let error = handler.event(event.clone()).await.unwrap_err();
    assert_eq!(error.kind, HostErrorKind::Protocol);
    assert_eq!(error.message, "Sidecar event payload is not an AgentEvent.");
    assert!(inner.events.lock().unwrap().is_empty());
    assert!(matches!(
        receiver.try_recv(),
        Err(mpsc::error::TryRecvError::Empty)
    ));
    println!("invalid payload {event:?}: {error}");
}

#[tokio::test]
async fn non_final_event_null_or_undefined_payload_is_protocol_error() {
    for payload in [JsValue::Null, JsValue::Undefined] {
        for explicit_final in [false, true] {
            let mut event = JsObject::new();
            event.insert("payload".into(), payload.clone());
            if explicit_final {
                event.insert("final".into(), JsValue::Bool(false));
            }
            assert_invalid_event_payload(JsValue::Object(event)).await;
        }
    }
}

#[tokio::test]
async fn non_final_event_non_object_payload_is_protocol_error() {
    for raw in [
        "false",
        "true",
        "0",
        "42",
        r#""""#,
        r#""event""#,
        "[]",
        "[{}]",
    ] {
        let payload = parse_json(raw).unwrap();
        for explicit_final in [false, true] {
            let mut event = JsObject::new();
            event.insert("payload".into(), payload.clone());
            if explicit_final {
                event.insert("final".into(), JsValue::Bool(false));
            }
            assert_invalid_event_payload(JsValue::Object(event)).await;
        }
    }
}

#[tokio::test]
async fn final_event_payload_shape_is_left_to_inner_handler() {
    for raw in [
        r#"{"final":true}"#,
        r#"{"final":true,"payload":null}"#,
        r#"{"final":true,"payload":false}"#,
        r#"{"final":true,"payload":[]}"#,
        r#"{"final":true,"payload":{}}"#,
    ] {
        let (inner, handler, mut receiver) = buffered_handler(4);
        let event = parse_json(raw).unwrap();
        handler.event(event.clone()).await.unwrap();
        assert_eq!(*inner.events.lock().unwrap(), vec![event]);
        assert!(matches!(
            receiver.try_recv(),
            Err(mpsc::error::TryRecvError::Empty)
        ));
    }
    // The exemption must still apply the terminal through the inner handler.
    let (events, mut receiver) = mpsc::channel(4);
    let handler = StreamHandler {
        inner: Arc::new(Handler {
            fail_event: true,
            ..Handler::default()
        }),
        events,
        integrity: Arc::new(StreamIntegrity::new()),
        forwarding: Arc::default(),
    };
    let error = handler
        .event(parse_json(r#"{"final":true}"#).unwrap())
        .await
        .unwrap_err();
    assert_eq!(error.kind, HostErrorKind::Callback);
    assert_eq!(error.message, "event failed");
    assert!(matches!(
        receiver.try_recv(),
        Err(mpsc::error::TryRecvError::Empty)
    ));
}

#[tokio::test]
async fn non_final_event_object_payload_is_applied_and_forwarded() {
    for payload in [host_event("model_event"), JsValue::Object(JsObject::new())] {
        for explicit_final in [false, true] {
            let (inner, handler, mut receiver) = buffered_handler(4);
            let mut event = JsObject::new();
            event.insert("payload".into(), payload.clone());
            if explicit_final {
                event.insert("final".into(), JsValue::Bool(false));
            }
            let event = JsValue::Object(event);
            handler.event(event.clone()).await.unwrap();
            assert_eq!(*inner.events.lock().unwrap(), vec![event]);
            assert_eq!(receiver.try_recv().unwrap(), payload);
            assert!(matches!(
                receiver.try_recv(),
                Err(mpsc::error::TryRecvError::Empty)
            ));
        }
    }
}

#[tokio::test]
async fn aborted_execution_releases_validated_wire_reservations() {
    let inner = Arc::new(Handler {
        block_input_accepted: true,
        ..Handler::default()
    });
    let (events, mut rx) = mpsc::channel(16);
    let handler = Arc::new(StreamHandler {
        inner: inner.clone(),
        events,
        integrity: Arc::new(StreamIntegrity::new()),
        forwarding: Arc::default(),
    });
    let client = spawn_client("live-normal").await.unwrap();
    let final_received = client.final_received();
    let child = client.child_id().unwrap();
    let (request, _) = parse_execute_request(&input(), &options(), vec![]).unwrap();
    let mut run = Box::pin(client.execute(request, handler.clone()));
    timeout(FINAL_SYNC_TIMEOUT, async {
        loop {
            tokio::select! {
                biased;
                result = &mut run => panic!("application must remain pending: {result:?}"),
                _ = tokio::task::yield_now() => {}
            }
            if final_received.load(Ordering::SeqCst) && inner.events.lock().unwrap().len() == 2 {
                break;
            }
        }
    })
    .await
    .expect("host must validate the queued events before abort");
    let reserved = handler.forwarding.lock().unwrap().wire.len();
    assert_eq!(reserved, 3);
    assert_eq!(inner.events.lock().unwrap().len(), 2); // input_accepted is pending
    assert_eq!(
        rx.try_recv().unwrap().get("type"),
        Some(&"turn_started".into())
    );
    // Construct a real host-buffered callback behind the queued wire slots.
    let module_event = host_event("after_aborted_execution");
    queue_host_events(&inner, std::slice::from_ref(&module_event));
    let mut callback = handler.module_call(JsValue::Null);
    assert_pending(&mut callback).await;
    drop(run); // the production execution future owns and drops the queue
    timeout(FINAL_SYNC_TIMEOUT, callback)
        .await
        .expect("discarded wire reservations must not strand a later module callback")
        .unwrap();
    assert!(handler.forwarding.lock().unwrap().wire.is_empty());
    assert_eq!(rx.try_recv().unwrap(), module_event);
    let later = host_event("after_aborted_wire");
    deliver(&handler, &later, false).await.unwrap();
    assert_eq!(rx.try_recv().unwrap(), later);
    timeout(
        FINAL_SYNC_TIMEOUT,
        handler.event(wire(&JsValue::Null, true)),
    )
    .await
    .expect("terminal callback must not wait on a discarded reservation")
    .unwrap();
    assert!(rx.try_recv().is_err());
    assert_reaped(child).await;
    println!(
        "queued=3; module blocked before abort; module/wire/final progressed after abort; wire=0"
    );
}

#[tokio::test]
async fn discarded_middle_wire_reservation_preserves_delivery_order() {
    for reserve_successor_first in [true, false] {
        let (_, handler, mut rx) = buffered_handler(8);
        let first = host_event("turn_started");
        let middle = host_event("input_accepted");
        let last = host_event("model_event");
        let first_guard = handler.validate_event(&first).unwrap();
        let middle_guard = handler.validate_event(&middle).unwrap();
        // Exercise both an existing successor and a later reservation via tail.
        let last_guard = if reserve_successor_first {
            let guard = handler.validate_event(&last).unwrap();
            drop(middle_guard);
            guard
        } else {
            drop(middle_guard);
            handler.validate_event(&last).unwrap()
        };
        let first_call = handler.event(wire(&first, false));
        let mut last_call = handler.event(wire(&last, false));
        // C's application is ready, but B's release must not let it overtake A.
        assert_pending(&mut last_call).await;
        assert!(matches!(
            rx.try_recv(),
            Err(mpsc::error::TryRecvError::Empty)
        ));
        timeout(FINAL_SYNC_TIMEOUT, first_call)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(rx.try_recv().unwrap(), first);
        timeout(FINAL_SYNC_TIMEOUT, last_call)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(rx.try_recv().unwrap(), last);
        assert!(matches!(
            rx.try_recv(),
            Err(mpsc::error::TryRecvError::Empty)
        ));
        assert!(handler.forwarding.lock().unwrap().wire.is_empty());
        drop((first_guard, last_guard));
    }
}

#[tokio::test]
async fn discarded_middle_wire_reservation_preserves_pending_module_order() {
    let (inner, handler, mut rx) = buffered_handler(8);
    let first = host_event("turn_started");
    let first_guard = handler.validate_event(&first).unwrap();
    let middle_guard = handler
        .validate_event(&host_event("input_accepted"))
        .unwrap();
    let last = host_event("module_after_discard");
    queue_host_events(&inner, std::slice::from_ref(&last));
    let mut last_call = handler.module_call(JsValue::Null);
    assert_pending(&mut last_call).await;
    drop(middle_guard); // C already owns B's completion receiver outside wire.
    assert_pending(&mut last_call).await;
    assert!(matches!(
        rx.try_recv(),
        Err(mpsc::error::TryRecvError::Empty)
    ));
    timeout(FINAL_SYNC_TIMEOUT, handler.event(wire(&first, false)))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(rx.try_recv().unwrap(), first);
    timeout(FINAL_SYNC_TIMEOUT, last_call)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(rx.try_recv().unwrap(), last);
    assert!(matches!(
        rx.try_recv(),
        Err(mpsc::error::TryRecvError::Empty)
    ));
    drop(first_guard);
}

#[tokio::test]
async fn discarded_middle_wire_reservation_keeps_predecessor_when_successor_is_cancelled() {
    let (inner, handler, mut rx) = buffered_handler(8);
    let first = host_event("turn_started");
    let first_guard = handler.validate_event(&first).unwrap();
    let middle_guard = handler
        .validate_event(&host_event("input_accepted"))
        .unwrap();
    queue_host_events(&inner, &[host_event("cancelled_module")]);
    let mut cancelled_call = handler.module_call(JsValue::Null);
    assert_pending(&mut cancelled_call).await;
    drop(middle_guard);
    assert_pending(&mut cancelled_call).await; // now waiting on A through B
    let last = host_event("module_after_cancel");
    queue_host_events(&inner, std::slice::from_ref(&last));
    let mut last_call = handler.module_call(JsValue::Null);
    drop(cancelled_call);
    assert_pending(&mut last_call).await;
    timeout(FINAL_SYNC_TIMEOUT, handler.event(wire(&first, false)))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(rx.try_recv().unwrap(), first);
    timeout(FINAL_SYNC_TIMEOUT, last_call)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(rx.try_recv().unwrap(), last);
    assert!(matches!(
        rx.try_recv(),
        Err(mpsc::error::TryRecvError::Empty)
    ));
    drop(first_guard);
}

#[test]
fn discarded_delivery_chain_drops_without_a_runtime_or_recursion() {
    for discard_tail_first in [false, true] {
        let mut forwarding = Forwarding::default();
        let slots: Vec<_> = (0..20_000).map(|_| forwarding.reserve()).collect();
        if discard_tail_first {
            drop(forwarding.tail.take());
        }
        // In forward order each cancelled slot relays the preceding chain.
        drop(slots);
        drop(forwarding);
    }
}

#[tokio::test]
async fn discarded_event_guards_release_only_their_own_reservation() {
    let (inner, handler, mut rx) = buffered_handler(8);
    let started = host_event("turn_started");
    let accepted = host_event("input_accepted");
    let first = handler.validate_event(&started).unwrap();
    let second = handler.validate_event(&accepted).unwrap();
    drop(second); // a later queue entry must not remove the earlier reservation
    let reserved = handler.forwarding.lock().unwrap().wire.len();
    assert_eq!(reserved, 1);
    queue_host_events(&inner, &[host_event("after_discard")]);
    let mut module = handler.module_call(JsValue::Null);
    assert_pending(&mut module).await;
    drop(first);
    timeout(FINAL_SYNC_TIMEOUT, module).await.unwrap().unwrap();
    assert!(handler.forwarding.lock().unwrap().wire.is_empty());
    assert_eq!(rx.try_recv().unwrap(), host_event("after_discard"));
    // A callback constructed but never polled must release its transferred slot.
    let guard = handler.validate_event(&host_event("model_event")).unwrap();
    let callback = handler.event(wire(&host_event("model_event"), false));
    drop(guard);
    drop(callback);
    timeout(
        FINAL_SYNC_TIMEOUT,
        handler.event(wire(&JsValue::Null, true)),
    )
    .await
    .unwrap()
    .unwrap();
}

#[tokio::test]
async fn embedded_final_uses_js_truthiness() {
    for value in [
        JsValue::Bool(true),
        "true".into(),
        "false".into(),
        JsValue::Number(1.0),
        JsValue::Number(-1.0),
        JsValue::Array(vec![]),
        JsValue::Object(JsObject::new()),
    ] {
        for payload in [
            None,
            Some(JsValue::Null),
            Some(JsValue::Bool(false)),
            Some(host_event("model_event")),
        ] {
            let (inner, handler, mut rx) = buffered_handler(4);
            let mut event = JsObject::new();
            event.insert("final".into(), value.clone());
            if let Some(payload) = payload {
                event.insert("payload".into(), payload);
            }
            let event = JsValue::Object(event);
            handler.event(event.clone()).await.unwrap();
            assert_eq!(*inner.events.lock().unwrap(), vec![event]);
            assert!(
                rx.try_recv().is_err(),
                "final payload must not be forwarded"
            );
        }
    }
    for value in [
        JsValue::Undefined,
        JsValue::Null,
        JsValue::Bool(false),
        JsValue::Number(0.0),
        JsValue::Number(-0.0),
        JsValue::Number(f64::NAN),
        "".into(),
    ] {
        let mut event = JsObject::new();
        event.insert("final".into(), value);
        event.insert("payload".into(), JsValue::Null);
        assert_invalid_event_payload(JsValue::Object(event.clone())).await;
        let (inner, handler, mut rx) = buffered_handler(4);
        let payload = host_event("model_event");
        event.insert("payload".into(), payload.clone());
        let event = JsValue::Object(event);
        handler.event(event.clone()).await.unwrap();
        let recorded = inner.events.lock().unwrap().clone();
        assert_eq!(recorded.len(), 1);
        if event
            .get("final")
            .and_then(JsValue::as_number)
            .is_some_and(f64::is_nan)
        {
            assert!(
                recorded[0]
                    .get("final")
                    .and_then(JsValue::as_number)
                    .unwrap()
                    .is_nan()
            );
            assert_eq!(recorded[0].get("payload"), Some(&payload));
        } else {
            assert_eq!(recorded, vec![event]);
        }
        assert_eq!(rx.try_recv().unwrap(), payload);
    }
}

// The TS rows constrain connection creation only. Rust's existing synthesized
// aborted terminal differs from TS (A7.2-TURN-ABORT-001, still unapproved).
async fn pre_cancelled_factory(dispose: bool, interaction: bool) {
    let oracle = crate::test_baseline::oracle(include_str!(
        "../../../tests/fixtures/turn-runner/oracle.json"
    ));
    let JsValue::Array(rows) = oracle.get("sidecar").unwrap() else {
        panic!("sidecar rows")
    };
    let row = rows
        .iter()
        .find(|row| {
            row.get("name")
                == Some(
                    &if dispose {
                        "dispose-before"
                    } else {
                        "abort-before"
                    }
                    .into(),
                )
        })
        .unwrap();
    assert_eq!(row.get("connections"), Some(&JsValue::Number(0.0)));
    let calls = Arc::new(AtomicUsize::new(0));
    let called = calls.clone();
    let session = if interaction {
        LiveSessionAdapter::with_interaction(test_recorder(), Vec::new, move |_, _| {
            called.fetch_add(1, Ordering::SeqCst);
            Box::pin(async {
                Err(HostError::new(
                    HostErrorKind::Callback,
                    "factory must not run",
                ))
            })
        })
    } else {
        LiveSessionAdapter::new(test_recorder(), Vec::new, move |_| {
            called.fetch_add(1, Ordering::SeqCst);
            Box::pin(async {
                Err(HostError::new(
                    HostErrorKind::Callback,
                    "factory must not run",
                ))
            })
        })
    };
    let mut turn = session.submit(input(), options()).unwrap();
    if dispose {
        session.dispose().await.unwrap();
    } else {
        session.abort(&"pre-cancel".into()).unwrap();
    }
    assert_aborted(&next(turn.as_mut()).await.unwrap().unwrap());
    assert_eq!(next(turn.as_mut()).await.unwrap(), None);
    assert_eq!(calls.load(Ordering::SeqCst), 0, "factory call must be lazy");
    assert!(session.state.lock().unwrap().active.is_none());
    assert_eq!(session.submit(input(), options()).is_err(), dispose);
}
#[tokio::test]
async fn live_factory_pre_aborted() {
    pre_cancelled_factory(false, false).await;
}
#[tokio::test]
async fn live_factory_pre_disposed() {
    pre_cancelled_factory(true, false).await;
}
#[tokio::test]
async fn live_interaction_factory_pre_aborted() {
    pre_cancelled_factory(false, true).await;
}
#[tokio::test]
async fn live_interaction_factory_pre_disposed() {
    pre_cancelled_factory(true, true).await;
}

async fn pending_factory(action: u8) {
    use std::task::{Context, Poll, Waker};
    struct Finished(Arc<AtomicUsize>);
    impl Drop for Finished {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }
    let calls = Arc::new(AtomicUsize::new(0));
    let finished = Arc::new(AtomicUsize::new(0));
    let called = calls.clone();
    let dropped = finished.clone();
    let (release, wait) = tokio::sync::oneshot::channel::<()>();
    let receiver = Mutex::new(Some(wait));
    let session = LiveSessionAdapter::new(test_recorder(), Vec::new, move |_| {
        called.fetch_add(1, Ordering::SeqCst);
        let wait = receiver
            .lock()
            .unwrap()
            .take()
            .expect("factory called once");
        let guard = Finished(dropped.clone());
        Box::pin(async move {
            let _guard = guard;
            wait.await
                .expect("test releases or cancels the owned factory");
            Err(HostError::new(HostErrorKind::Callback, "factory finished"))
        })
    });
    let mut turn = session.submit(input(), options()).unwrap();
    // No timer resolves this predecessor. Poll each borrowed next once, then
    // cancel that waiter. Both must keep the same owned factory and active slot.
    for _ in 0..2 {
        let mut waiter = turn.next();
        assert!(matches!(
            waiter
                .as_mut()
                .poll(&mut Context::from_waker(Waker::noop())),
            Poll::Pending
        ));
        drop(waiter);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(finished.load(Ordering::SeqCst), 0);
        assert!(session.submit(input(), options()).is_err());
    }
    match action {
        0 | 1 => {
            if action == 0 {
                session.abort(&"pending".into()).unwrap();
            } else {
                session.dispose().await.unwrap();
            }
            // next() helper bounds cancellation at fifteen seconds; the factory
            // never receives release, so cancellation is the only exit.
            assert_aborted(&next(turn.as_mut()).await.unwrap().unwrap());
            assert_eq!(next(turn.as_mut()).await.unwrap(), None);
            assert!(release.send(()).is_err(), "pending factory was dropped");
        }
        2 => {
            drop(turn);
            assert!(release.send(()).is_err());
        }
        _ => {
            release.send(()).unwrap();
            assert!(
                next(turn.as_mut())
                    .await
                    .unwrap_err()
                    .message
                    .contains("factory finished")
            );
            assert_eq!(next(turn.as_mut()).await.unwrap(), None);
        }
    }
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(finished.load(Ordering::SeqCst), 1);
    assert!(session.state.lock().unwrap().active.is_none());
    assert_eq!(session.submit(input(), options()).is_err(), action == 1);
}
#[tokio::test]
async fn live_factory_pending_abort() {
    pending_factory(0).await;
}
#[tokio::test]
async fn live_factory_pending_dispose() {
    pending_factory(1).await;
}
#[tokio::test]
async fn live_factory_pending_drop() {
    pending_factory(2).await;
}
#[tokio::test]
async fn live_factory_cancelled_waiter_resumes() {
    pending_factory(3).await;
}

#[test]
fn live_input_matches_ts_original_history_synthetic_and_metadata() {
    let oracle = crate::test_baseline::oracle(include_str!(
        "../../../tests/fixtures/turn-runner/oracle.json"
    ));
    let JsValue::Array(rows) = oracle.get("matrix").unwrap() else {
        panic!("matrix")
    };
    let expected = rows
        .iter()
        .filter(|row| row.get("rawInput").is_some())
        .count();
    assert!(expected > 0, "oracle must contain rawInput cases");
    let mut visited = 0;
    for row in rows.iter().filter(|row| row.get("rawInput").is_some()) {
        visited += 1;
        let array = |key| match row.get(key).unwrap() {
            JsValue::Array(values) => values.clone(),
            _ => panic!("{key}"),
        };
        let mut opts = options().as_object().unwrap().clone();
        for (key, value) in row
            .get("submitOptions")
            .unwrap()
            .as_object()
            .unwrap()
            .entries()
        {
            opts.insert(key.clone(), value.clone());
        }
        opts.insert(
            "syntheticMessages".into(),
            row.get("synthetic").unwrap().clone(),
        );
        let (request, prepared) = parse_execute_request(
            row.get("rawInput").unwrap(),
            &JsValue::Object(opts),
            array("history"),
        )
        .unwrap();
        assert_eq!(
            request.payload.get("messages"),
            row.get("loopMessages"),
            "TS loop input: {:?}",
            row.get("name")
        );
        assert!(
            !request.payload.contains_key("syntheticMessages"),
            "synthetic must not be applied twice"
        );
        let transcript = row
            .get("transcript")
            .unwrap()
            .as_string()
            .unwrap()
            .to_utf8()
            .unwrap();
        let accepted = transcript
            .lines()
            .map(|line| parse_json(line).unwrap())
            .find(|e| e.get("type") == Some(&"accepted_input".into()))
            .unwrap();
        assert_eq!(
            prepared.metadata.map(JsValue::Object),
            accepted.get("metadata").cloned(),
            "TS metadata"
        );
    }
    assert_eq!(
        visited, expected,
        "all rawInput oracle cases must be visited"
    );
}

async fn live_input_shared_recorder(interaction: bool) {
    let recorder = test_recorder();
    let supplied = recorder.clone();
    let observed = Arc::new(AtomicUsize::new(0));
    let calls = observed.clone();
    let check = move |shared: Arc<Recorder>| {
        assert!(
            Arc::ptr_eq(&shared, &supplied),
            "factory must receive the session Recorder"
        );
        assert!(
            Arc::ptr_eq(shared.writer(), supplied.writer()),
            "durable ports share writer"
        );
        calls.fetch_add(1, Ordering::SeqCst);
        // Exercise the model/tool recording surface against the same turn state.
        shared
            .start_turn(&"session".into(), &"turn".into())
            .unwrap();
        shared
            .record_model_request(
                &"session".into(),
                &"turn".into(),
                parse_json(r#"{"messages":[]}"#).unwrap(),
            )
            .unwrap();
        shared
            .record_tool_calls(
                &"session".into(),
                &"turn".into(),
                &[parse_json(r#"{"id":"call","name":"probe","input":{}}"#).unwrap()],
            )
            .unwrap();
    };
    let session = if interaction {
        LiveSessionAdapter::with_interaction(recorder.clone(), Vec::new, move |_, shared| {
            check(shared);
            Box::pin(async { Err(HostError::new(HostErrorKind::Callback, "observed recorder")) })
        })
    } else {
        LiveSessionAdapter::new(recorder.clone(), Vec::new, move |shared| {
            check(shared);
            Box::pin(async { Err(HostError::new(HostErrorKind::Callback, "observed recorder")) })
        })
    };
    let mut turn = session.submit(input(), options()).unwrap();
    assert!(
        next(turn.as_mut())
            .await
            .unwrap_err()
            .message
            .contains("observed recorder")
    );
    assert_eq!(observed.load(Ordering::SeqCst), 1, "factory reached");
    assert_eq!(
        recorder.snapshot_state().0,
        4,
        "one shared sequence for start/step/model/tool"
    );
    let bytes = std::fs::read_to_string(recorder.writer().path()).unwrap();
    let entries: Vec<_> = bytes
        .lines()
        .map(|line| parse_json(line).unwrap())
        .collect();
    for pair in entries.windows(2) {
        assert_eq!(
            pair[1].get("parentEntryId"),
            pair[0].get("entryId"),
            "shared parent chain"
        );
    }
    std::fs::remove_dir_all(recorder.writer().path().parent().unwrap()).unwrap();
}

#[tokio::test]
async fn live_input_new_factory_shares_recorder() {
    live_input_shared_recorder(false).await;
}
#[tokio::test]
async fn live_input_interaction_factory_shares_recorder() {
    live_input_shared_recorder(true).await;
}

#[tokio::test]
async fn live_input_reads_current_history_on_every_submit() {
    let history = Arc::new(Mutex::new(vec![]));
    let current = history.clone();
    let session = LiveSessionAdapter::new(
        test_recorder(),
        move || current.lock().unwrap().clone(),
        |_| {
            Box::pin(async {
                Ok((
                    Box::pin(spawn_client("live-payload")) as HostFuture<HostClient>,
                    Arc::new(Handler::default()) as Arc<dyn HostHandler>,
                    test_lease(),
                ))
            })
        },
    );
    for text in ["first", "updated"] {
        let message = parse_json(&format!(
            r#"{{"role":"assistant","content":[{{"type":"text","text":"{text}"}}]}}"#
        ))
        .unwrap();
        *history.lock().unwrap() = vec![message.clone()];
        let mut turn = session.submit(input(), turn_options(text)).unwrap();
        next(turn.as_mut()).await.unwrap();
        next(turn.as_mut()).await.unwrap().unwrap();
        let completed = next(turn.as_mut()).await.unwrap().unwrap();
        let messages = completed.get("messages").unwrap();
        assert_eq!(
            messages,
            &JsValue::Array(vec![
                message,
                parse_json(
                    r#"{"role":"user","content":[{"type":"text","text":"original\ud800"}]}"#
                )
                .unwrap()
            ]),
            "fresh history per submit"
        );
        while next(turn.as_mut()).await.unwrap().is_some() {}
    }
    let mut opts = options().as_object().unwrap().clone();
    let synthetic =
        parse_json(r#"{"role":"user","content":[{"type":"text","text":"synthetic"}]}"#).unwrap();
    opts.insert(
        "syntheticMessages".into(),
        JsValue::Array(vec![synthetic.clone()]),
    );
    let mut turn = session.submit(input(), JsValue::Object(opts)).unwrap();
    session.abort(&"pre-cancel".into()).unwrap();
    let aborted = next(turn.as_mut()).await.unwrap().unwrap();
    let JsValue::Array(ref expected) = input_messages() else {
        unreachable!()
    };
    let mut expected = expected.clone();
    expected.insert(0, history.lock().unwrap()[0].clone());
    expected.push(synthetic);
    assert_eq!(
        aborted.get("messages"),
        Some(&JsValue::Array(expected)),
        "abort preserves history and synthetic"
    );
}

#[test]
fn live_input_nullish_synthetic_is_empty() {
    for value in [JsValue::Null, JsValue::Undefined] {
        let mut opts = options().as_object().unwrap().clone();
        opts.insert("syntheticMessages".into(), value);
        let (request, prepared) =
            parse_execute_request(&input(), &JsValue::Object(opts), vec![]).unwrap();
        assert!(prepared.synthetic.is_empty(), "nullish synthetic is empty");
        assert_eq!(request.payload.get("messages"), Some(&input_messages()));
    }
}

fn test_lease() -> Arc<RuntimeLease> {
    use crate::runtime::project::{ProjectRuntimeRegistry, ResourceBundle};
    let registry = ProjectRuntimeRegistry::new(
        std::env::temp_dir(),
        Arc::new(|_: &std::path::Path, _: &mut ResourceBundle| Ok(())),
    );
    Arc::new(registry.acquire(None).unwrap())
}

/// The gate is inside the production blocking append, not an owner test guard.
struct RecordingStore {
    disk: crate::storage::JsonlTranscriptStore,
    kind: &'static str,
    fail: usize,
    completes: AtomicUsize,
    attempts: AtomicUsize,
    gate: Mutex<Option<(oneshot::Sender<()>, std::sync::mpsc::Receiver<()>)>>,
}
impl crate::storage::TranscriptStore for RecordingStore {
    fn append(
        &self,
        key: &crate::storage::TranscriptKey,
        entry: &JsValue,
    ) -> crate::storage::StoreResult<()> {
        if entry.get("type") == Some(&"turn_result".into()) {
            self.completes.fetch_add(1, Ordering::SeqCst);
        }
        if entry.get("type") == Some(&self.kind.into()) {
            self.attempts.fetch_add(1, Ordering::SeqCst);
            if let Some((entered, release)) = self.gate.lock().unwrap().take() {
                let _ = entered.send(());
                release
                    .recv_timeout(Duration::from_secs(10))
                    .expect("test must release blocking I/O");
            }
            if self.attempts.load(Ordering::SeqCst) <= self.fail {
                return Err(std::io::Error::other("recording write failed").into());
            }
        }
        self.disk.append(key, entry)
    }
    fn read(
        &self,
        key: &crate::storage::TranscriptKey,
        limit: Option<u64>,
    ) -> crate::storage::StoreResult<crate::storage::TranscriptRead> {
        self.disk.read(key, limit)
    }
}

fn assert_recording_pending<F: std::future::Future + ?Sized>(
    future: std::pin::Pin<&mut F>,
    reason: &str,
) {
    use std::task::{Context, Waker};
    assert!(
        future
            .poll(&mut Context::from_waker(Waker::noop()))
            .is_pending(),
        "{reason}"
    );
}

async fn live_recording_matrix(phase: usize, action: &str) {
    use crate::{
        runtime::project::{ProjectRuntimeRegistry, ResourceBundle},
        storage::{JsonlTranscriptStore, TranscriptKey},
    };
    let oracle_action = if action == "failure" {
        "cancel"
    } else {
        action
    };
    let name = format!("{}-{oracle_action}", ["start", "accept", "complete"][phase]);
    let oracle = crate::test_baseline::oracle(include_str!(
        "../../../tests/fixtures/live-recording/oracle.json"
    ));
    let JsValue::Array(rows) = oracle.get("matrix").unwrap() else {
        panic!("matrix")
    };
    let row = rows
        .iter()
        .find(|r| r.get("name") == Some(&name.as_str().into()))
        .unwrap();
    let root = std::env::temp_dir().join(format!("pdrs-live-recording-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&root).unwrap();
    let key = TranscriptKey {
        project_root: root.clone(),
        pilot_home: root.join("home"),
        session_id: "session".into(),
        transcript_path: root.join("session.jsonl"),
    };
    let registry = Arc::new(ProjectRuntimeRegistry::new(
        &root,
        Arc::new(|_: &std::path::Path, _: &mut ResourceBundle| Ok(())),
    ));
    let runtime = registry.resolve(None).unwrap();
    let (entered, mut entry) = oneshot::channel();
    let (release, wait) = std::sync::mpsc::channel();
    let store = Arc::new(RecordingStore {
        disk: JsonlTranscriptStore::new(),
        kind: ["turn_started", "accepted_input", "turn_result"][phase],
        fail: usize::from(action == "failure"),
        completes: AtomicUsize::new(0),
        attempts: AtomicUsize::new(0),
        gate: Mutex::new(Some((entered, wait))),
    });
    let recorder = Arc::new(Recorder::new(store.clone(), key.clone()));
    let owner = registry.clone();
    let session = LiveSessionAdapter::new(recorder, Vec::new, move |_| {
        let lease = Arc::new(owner.acquire(None).unwrap());
        Box::pin(async move {
            Ok((
                Box::pin(spawn_client("live-payload")) as HostFuture<HostClient>,
                Arc::new(Handler::default()) as Arc<dyn HostHandler>,
                lease,
            ))
        })
    });
    let mut turn = session.submit(input(), options()).unwrap();
    for _ in 0..phase {
        next(turn.as_mut()).await.unwrap().unwrap();
    }
    {
        let mut waiting = turn.next();
        timeout(Duration::from_secs(15), async {
            tokio::select! { biased;
                _ = &mut entry => {},
                result = &mut waiting => panic!("recording gate must precede delivery: {result:?}"),
            }
        })
        .await
        .expect("append must enter the gate");
    } // Cancel only next's borrow. The same driver must remain owned.
    let count = || {
        std::fs::read_to_string(&key.transcript_path)
            .unwrap_or_default()
            .lines()
            .filter(|line| parse_json(line).unwrap().get("type") == Some(&store.kind.into()))
            .count()
    };
    let active = || session.state.lock().unwrap().active.is_some();
    let rejected = || session.submit(input(), options()).is_err();
    assert_eq!(
        row.get("before").unwrap().get("active"),
        Some(&JsValue::Number(f64::from(active())))
    );
    assert_eq!(count(), 0, "write awaits release");
    assert_eq!(store.attempts.load(Ordering::SeqCst), 1, "one I/O entered");
    assert!(active(), "stream owns real active slot");
    for _ in 0..2 {
        let mut waiting = turn.next();
        assert_recording_pending(waiting.as_mut(), "pending predecessor");
    } // A second cancelled waiter cannot restart or discard the first write.
    assert!(rejected(), "pending write blocks resubmit");
    let mut disposal = Box::pin(registry.dispose());
    if action == "dispose" {
        session.dispose().await.unwrap();
        assert!(rejected(), "disposed session rejects admission");
        assert_recording_pending(disposal.as_mut(), "factory waits for I/O");
    }
    if action == "drop" || action == "dispose" {
        drop(turn);
        assert!(active(), "worker owns active slot");
        assert_eq!(runtime.lease_count(), 1, "worker retains lease");
        assert!(rejected(), "worker blocks resubmit");
        if action == "dispose" {
            assert_recording_pending(disposal.as_mut(), "factory waits for worker");
        }
        release.send(()).unwrap();
    } else if action == "failure" {
        release.send(()).unwrap();
        if phase == 0 {
            assert!(
                next(turn.as_mut())
                    .await
                    .unwrap_err()
                    .message
                    .contains("recording write failed")
            );
        } else {
            let error = persisted_failure(turn.as_mut()).await;
            assert!(error.contains(if phase == 1 {
                "Failed to record accepted input."
            } else {
                "recording write failed"
            }));
        }
        assert_eq!(
            next(turn.as_mut()).await.unwrap(),
            None,
            "failed stream closes"
        );
        drop(turn);
    } else {
        release.send(()).unwrap();
        let event = next(turn.as_mut()).await.unwrap().unwrap();
        assert_eq!(event.get("type"), row.get("event"), "same transition event");
        let mut expected = JsObject::new();
        expected.insert("type".into(), row.get("event").unwrap().clone());
        expected.insert("sessionId".into(), "session".into());
        expected.insert("turnId".into(), "turn".into());
        if phase == 1 {
            expected.insert("messages".into(), input_messages());
        } else if phase == 2 {
            expected.insert("result".into(), parse_json(r#"{"type":"success","sessionId":"session","turnId":"turn","stopReason":"completed","turns":1}"#).unwrap());
            expected.insert("messages".into(), input_messages());
        }
        assert_eq!(
            event,
            JsValue::Object(expected),
            "original payload survives cancelled next"
        );
        assert_eq!(store.attempts.load(Ordering::SeqCst), 1, "one I/O");
        if phase == 2 {
            assert!(!active(), "EOF stream releases slot");
            assert_eq!(
                next(turn.as_mut()).await.unwrap(),
                None,
                "terminal delivered once"
            );
        } else {
            assert!(rejected(), "runner still owns live slot");
        }
        drop(turn);
    }
    // The worker owns the slot until append returns; explicit release above
    // is the progress condition, timeout only fails the test (never unlocks).
    timeout(Duration::from_secs(15), async {
        while active() || runtime.lease_count() != 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("write and final active owner must exit");
    assert_eq!(runtime.lease_count(), 0, "write exited");
    let writes = usize::from(action != "failure" || phase == 2);
    assert_eq!(count(), writes, "one durable write");
    if action != "failure" {
        assert_eq!(
            row.get("after").unwrap().get("writes"),
            Some(&JsValue::Number(count() as f64))
        );
    }
    if action == "dispose" {
        assert!(
            session.submit(input(), turn_options("next")).is_err(),
            "disposed session stays closed"
        );
        assert_eq!(row.get("replacementStarted"), Some(&JsValue::Bool(false)));
    } else {
        let mut replacement = session.submit(input(), turn_options("next")).unwrap();
        let event = next(replacement.as_mut()).await.unwrap().unwrap();
        assert_eq!(
            event.get("type"),
            Some(&"turn_started".into()),
            "fresh turn starts"
        );
        if action != "failure" {
            assert_eq!(row.get("replacementStarted"), Some(&JsValue::Bool(true)));
        }
        drop(replacement);
    }
    timeout(Duration::from_secs(15), &mut disposal)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        runtime.state(),
        crate::runtime::project::RuntimeState::Disposed
    );
    std::fs::remove_dir_all(root).unwrap();
}
fn assert_fixture_consumed(rows: &[JsValue], expected: impl Iterator<Item = String>) {
    let mut actual: Vec<_> = rows
        .iter()
        .map(|row| {
            row.get("name")
                .unwrap()
                .as_string()
                .unwrap()
                .to_utf8()
                .unwrap()
        })
        .collect();
    let mut expected: Vec<_> = expected.collect();
    actual.sort();
    expected.sort();
    assert_eq!(
        actual, expected,
        "oracle rows must be fully consumed exactly once"
    );
}

macro_rules! live_recording_cases {
    ($($name:ident, $phase:literal, $action:literal;)*) => {
        $(#[tokio::test]
        async fn $name() {
            live_recording_matrix($phase, $action).await;
        })*
        #[test]
        fn live_recording_oracle_fully_consumed() {
            let oracle = crate::test_baseline::oracle(include_str!(
                "../../../tests/fixtures/live-recording/oracle.json"
            ));
            let JsValue::Array(rows) = oracle.get("matrix").unwrap() else { panic!("matrix") };
            assert_fixture_consumed(rows, [$(($phase, $action)),*].into_iter()
                .filter(|(_, action)| *action != "failure")
                .map(|(phase, action)| format!("{}-{action}", ["start", "accept", "complete"][phase])));
        }
    };
}
live_recording_cases! {
    live_recording_start_cancel, 0, "cancel";
    live_recording_start_drop, 0, "drop";
    live_recording_start_resubmit, 0, "resubmit";
    live_recording_start_dispose, 0, "dispose";
    live_recording_start_failure, 0, "failure";
    live_recording_accept_cancel, 1, "cancel";
    live_recording_accept_drop, 1, "drop";
    live_recording_accept_resubmit, 1, "resubmit";
    live_recording_accept_dispose, 1, "dispose";
    live_recording_accept_failure, 1, "failure";
    live_recording_complete_cancel, 2, "cancel";
    live_recording_complete_drop, 2, "drop";
    live_recording_complete_resubmit, 2, "resubmit";
    live_recording_complete_dispose, 2, "dispose";
    live_recording_complete_failure, 2, "failure";
}
#[test]
#[ignore = "requires fixed PDRS_BASELINE with installed TS dependencies"]
fn live_recording_regenerates_identically() {
    crate::test_baseline::regenerates_identically("live-recording");
}

#[tokio::test]
async fn live_history_slot_matrix() {
    let oracle = crate::test_baseline::oracle(include_str!(
        "../../../tests/fixtures/live-history/oracle.json"
    ));
    let JsValue::Array(rows) = oracle.get("matrix").unwrap() else {
        panic!("matrix")
    };
    assert_eq!(rows.len(), 5);
    let reads = Arc::new(AtomicUsize::new(0));
    let observed = reads.clone();
    let history = Arc::new(Mutex::new(Vec::new()));
    let current = history.clone();
    let (entered_tx, entered_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let release_rx = Mutex::new(release_rx);
    let factory_calls = Arc::new(AtomicUsize::new(0));
    let calls = factory_calls.clone();
    let session = Arc::new(LiveSessionAdapter::new(
        test_recorder(),
        move || {
            if observed.fetch_add(1, Ordering::SeqCst) == 0 {
                entered_tx.send(()).unwrap();
                release_rx
                    .lock()
                    .unwrap()
                    .recv_timeout(Duration::from_secs(3))
                    .unwrap();
            }
            current.lock().unwrap().clone()
        },
        move |_| {
            calls.fetch_add(1, Ordering::SeqCst);
            Box::pin(async { std::future::pending().await })
        },
    ));
    let submitting = session.clone();
    let first = std::thread::spawn(move || submitting.submit(input(), turn_options("first")));
    entered_rx.recv_timeout(Duration::from_secs(3)).unwrap();
    let mut seen = Vec::new();
    let observe = |name: &str| {
        crate::metadata::object([
            ("name", name.into()),
            (
                "active",
                JsValue::Number(usize::from(session.state.lock().unwrap().active.is_some()) as f64),
            ),
            (
                "reads",
                JsValue::Number(reads.load(Ordering::SeqCst) as f64),
            ),
        ])
    };
    let rejected = session.submit(input(), turn_options("history-owned")).err();
    seen.push(observe("history-owned"));
    // Release and join before asserting: a failed negative cannot strand its thread.
    release_tx.send(()).unwrap();
    let mut first = first.join().unwrap().unwrap();
    assert_eq!(
        rejected.map(|e| e.message).as_deref(),
        Some("Agent session already has an active turn."),
        "concurrent submit must reject before reading history"
    );
    // Enter the never-completing factory before pending/cancelled admission checks.
    let mut pending = first.next();
    std::future::poll_fn(|cx| {
        assert!(pending.as_mut().poll(cx).is_pending());
        std::task::Poll::Ready(())
    })
    .await;
    drop(pending);
    assert_eq!(
        factory_calls.load(Ordering::SeqCst),
        1,
        "pending predecessor reached factory"
    );
    for name in ["pending", "aborted"] {
        if name == "aborted" {
            session.abort(&"cancel".into()).unwrap();
        }
        let error = session.submit(input(), turn_options(name)).err();
        assert_eq!(
            error.map(|e| e.message).as_deref(),
            Some("Agent session already has an active turn.")
        );
        seen.push(observe(name));
    }
    drop(first);
    let fresh = rows
        .iter()
        .find(|row| row.get("name") == Some(&"released".into()))
        .expect("released history row")
        .get("messages")
        .unwrap()
        .clone();
    let JsValue::Array(messages) = &fresh else {
        panic!("messages")
    };
    *history.lock().unwrap() = messages.clone();
    let mut replacement = session
        .submit(input(), turn_options("replacement"))
        .unwrap();
    session.abort(&"cancel before factory".into()).unwrap();
    let active_at_read = usize::from(session.state.lock().unwrap().active.is_some());
    let aborted = next(replacement.as_mut()).await.unwrap().unwrap();
    let JsValue::Array(actual) = aborted.get("messages").unwrap() else {
        panic!("messages")
    };
    let mut expected_messages = messages.clone();
    let JsValue::Array(ref original_input) = input_messages() else {
        unreachable!()
    };
    expected_messages.extend(original_input.iter().cloned());
    assert_eq!(*actual, expected_messages, "new owner reads fresh history");
    let observed_history = JsValue::Array(actual[..messages.len()].to_vec());
    // The cancellation terminal releases ownership when the stream is drained.
    seen.push(crate::metadata::object([
        ("name", "released".into()),
        ("active", JsValue::Number(active_at_read as f64)),
        (
            "reads",
            JsValue::Number(reads.load(Ordering::SeqCst) as f64),
        ),
        ("messages", observed_history),
    ]));
    drop(replacement);
    session.dispose().await.unwrap();
    assert!(session.submit(input(), turn_options("disposed")).is_err());
    seen.push(crate::metadata::object([
        ("name", "disposed".into()),
        (
            "active",
            JsValue::Number(usize::from(session.state.lock().unwrap().active.is_some()) as f64),
        ),
        (
            "reads",
            JsValue::Number(reads.load(Ordering::SeqCst) as f64),
        ),
    ]));
    assert_eq!(seen, *rows, "history slot matrix must match real TS");
}

#[test]
#[ignore = "requires fixed PDRS_BASELINE with installed TS dependencies"]
fn live_history_regenerates_identically() {
    crate::test_baseline::regenerates_identically("live-history");
}

#[path = "gates_tests.rs"]
mod gates_tests;

// A prepared handler must stop even when dispose never completes. Virtual time
// distinguishes both shorter and longer configured budgets from the old 5 s.
#[tokio::test]
async fn live_prepared_cleanup_uses_close_timeout() {
    struct PendingDispose {
        calls: Arc<AtomicUsize>,
        entered: Arc<tokio::sync::Notify>,
        done: Mutex<Option<oneshot::Sender<()>>>,
    }
    struct Exited(Option<oneshot::Sender<()>>);
    impl Drop for Exited {
        fn drop(&mut self) {
            let _ = self.0.take().unwrap().send(());
        }
    }
    impl HostHandler for PendingDispose {
        fn module_call(&self, _: JsValue) -> HostFuture<JsObject> {
            Box::pin(async { panic!("unexecuted client") })
        }
        fn dispose(&self) -> HostFuture<()> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let guard = Exited(self.done.lock().unwrap().take());
            self.entered.notify_one();
            Box::pin(async move {
                let _guard = guard;
                std::future::pending().await
            })
        }
    }
    for budget in [Duration::from_millis(7), Duration::from_secs(7)] {
        use crate::runtime::{
            project::{ProjectRuntimeRegistry, ResourceBundle},
            turn::host_factory::{Deployment, TurnHostFactory},
        };
        let registry = Arc::new(ProjectRuntimeRegistry::new(
            std::env::temp_dir(),
            Arc::new(|_: &std::path::Path, _: &mut ResourceBundle| Ok(())),
        ));
        let configured = TurnHostFactory::new(
            registry,
            Deployment::Native,
            Limits {
                close_timeout: budget,
                ..Limits::default()
            },
            None,
            |_, _| panic!("factory must remain lazy"),
        )
        .into_live_session(test_recorder(), Vec::new);
        assert_eq!(
            configured.close_timeout, budget,
            "factory forwards close_timeout"
        );
        let calls = Arc::new(AtomicUsize::new(0));
        let entered = Arc::new(tokio::sync::Notify::new());
        let (done, mut exited) = oneshot::channel();
        let handler = Arc::new(PendingDispose {
            calls: calls.clone(),
            entered: entered.clone(),
            done: Mutex::new(Some(done)),
        });
        let session = LiveSessionAdapter::new(test_recorder(), Vec::new, move |_| {
            let handler = handler.clone();
            Box::pin(async move {
                let connect: HostFuture<HostClient> =
                    Box::pin(async { panic!("connection must remain lazy") });
                Ok((connect, handler as Arc<dyn HostHandler>, test_lease()))
            })
        })
        .with_close_timeout(budget);
        let mut turn = session.submit(input(), options()).unwrap();
        assert_eq!(
            next(turn.as_mut()).await.unwrap().unwrap().get("type"),
            Some(&"turn_started".into())
        );
        tokio::time::pause();
        drop(turn); // No accept/execute; TurnScope owns the never-ending dispose.
        timeout(Duration::from_secs(15), entered.notified())
            .await
            .unwrap();
        tokio::time::advance(budget - Duration::from_millis(1)).await;
        tokio::task::yield_now().await;
        assert_eq!(
            exited.try_recv(),
            Err(oneshot::error::TryRecvError::Empty),
            "cleanup must retain the configured budget"
        );
        tokio::time::advance(Duration::from_millis(2)).await;
        assert!(
            timeout(Duration::from_millis(1), &mut exited).await.is_ok(),
            "cleanup must stop at the configured deadline"
        );
        tokio::time::resume();
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "prepared dispose exactly once"
        );
        assert!(
            session.state.lock().unwrap().active.is_none(),
            "cleanup releases slot"
        );
    }
}

#[tokio::test]
async fn sidecar_payload_embedded_reader_matches_ts() {
    let oracle = crate::test_baseline::oracle(include_str!(
        "../../../tests/fixtures/sidecar-payload/oracle.json"
    ));
    let JsValue::Array(cases) = oracle.get("cases").unwrap() else {
        panic!("cases")
    };
    let mut replayed = 0;
    for case in cases {
        if case.get("route") != Some(&"embedded-reader".into()) {
            continue;
        }
        replayed += 1;
        let mut event = JsObject::new();
        if let Some(payload) = case.get("payload") {
            event.insert(
                "payload".into(),
                if payload.get("__pdrsUndefined") == Some(&JsValue::Bool(true)) {
                    JsValue::Undefined
                } else {
                    payload.clone()
                },
            );
        }
        let (inner, handler, mut rx) = buffered_handler(4);
        let result = handler.event(JsValue::Object(event)).await;
        assert_eq!(
            result.is_ok(),
            case.get("ok").unwrap().as_bool().unwrap(),
            "{case:?}"
        );
        if let Err(error) = result {
            assert_eq!(error.kind, HostErrorKind::Protocol);
            assert_eq!(
                JsValue::from(error.message.as_str()),
                *case.get("error").unwrap()
            );
            assert!(inner.events.lock().unwrap().is_empty());
            assert!(rx.try_recv().is_err());
        } else {
            assert_eq!(inner.events.lock().unwrap().len(), 1);
            assert_eq!(rx.try_recv().unwrap(), *case.get("value").unwrap());
        }
        assert!(handler.forwarding.lock().unwrap().wire.is_empty());
    }
    assert_eq!(replayed, 12);
}
