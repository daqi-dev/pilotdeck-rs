use super::*;
use crate::host::{Limits, POST_CANCEL_EXIT_TIMEOUT};
use crate::runtime::turn::projector::{
    GatewayAgentEventProjectionInput, GatewayAgentEventProjector,
};
use std::{
    sync::atomic::{AtomicUsize, Ordering},
    time::Duration,
};
use tokio::{process::Command, time::timeout};

#[derive(Default)]
struct Handler {
    events: Mutex<Vec<JsValue>>,
    calls: AtomicUsize,
    disposed: AtomicUsize,
    fail_event: bool,
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

    fn validate_event(&self, payload: &JsValue) -> Result<(), HostError> {
        if self.reject_input_accepted && payload.get("type") == Some(&"input_accepted".into()) {
            return Err(HostError::new(
                HostErrorKind::Protocol,
                "inner rejected input_accepted",
            ));
        }
        Ok(())
    }

    fn dispose(&self) -> HostFuture<()> {
        self.disposed.fetch_add(1, Ordering::SeqCst);
        Box::pin(async { Ok(()) })
    }
}

fn options() -> JsValue {
    parse_json(r#"{"sessionId":"session","turnId":"turn","runId":"run","operationId":"operation","requestId":"request"}"#).unwrap()
}

fn input() -> JsValue {
    parse_json(r#"{"messages":[{"role":"user","content":[{"type":"text","text":"original\ud800"}],"custom":7}]}"#).unwrap()
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
        identity: parse_execute_request(&input(), &options())
            .unwrap()
            .identity,
        input: input(),
        integrity,
        delivered: StreamIntegrity::new(),
        active: None,
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
    if !final_event {
        handler.validate_event(payload)?;
    }
    handler.event(wire(payload, final_event)).await
}

async fn next(turn: &mut dyn LiveTurn) -> Result<Option<JsValue>, RuntimeError> {
    timeout(Duration::from_secs(15), turn.next())
        .await
        .expect("LiveTurn must make progress")
}

fn assert_aborted(event: &JsValue) {
    assert_eq!(event.get("type"), Some(&"turn_completed".into()));
    assert_eq!(event.get("sessionId"), Some(&"session".into()));
    assert_eq!(event.get("turnId"), Some(&"turn".into()));
    assert_eq!(event.get("messages"), input().get("messages"));
    let result = event.get("result").unwrap();
    for (key, expected) in [
        ("type", "aborted".into()),
        ("sessionId", "session".into()),
        ("turnId", "turn".into()),
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
    // Pre-connect the first test client so abort-before-execute still exercises
    // HostClient's cleanup. Later turns must create their own connection.
    let initial = Mutex::new(Some(spawn_client(mode).await.unwrap()));
    let mode = mode.to_owned();
    LiveSessionAdapter::new(move || {
        let initial = initial.lock().unwrap().take();
        let mode = mode.clone();
        // This test double only records lifecycle calls; it owns no modules.
        let handler = handler.clone();
        Box::pin(async move {
            let client = match initial {
                Some(client) => client,
                None => spawn_client(&mode).await?,
            };
            Ok((client, handler as Arc<dyn HostHandler>))
        })
    })
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
    let adapter = LiveSessionAdapter::new(move || {
        let tracked = tracked.clone();
        let mode = mode.clone();
        let handler = handler.clone();
        Box::pin(async move {
            let client = spawn_client(&mode).await?;
            if let Some(pid) = client.child_id() {
                tracked.lock().unwrap().push(pid);
            }
            Ok((client, handler as Arc<dyn HostHandler>))
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
        input().get("messages"),
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
async fn host_early_failure_completes_before_input_accepted() {
    let handler = Arc::new(Handler::default());
    let session = session("live-early-failure", handler.clone()).await;
    let mut turn = session.submit(input(), options()).unwrap();
    for event_type in ["turn_started", "agent_status", "turn_failed"] {
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
        input().get("messages"),
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
        input().get("messages"),
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
    // turn_started's application may still be in flight when the rejection
    // fails the execution, so only the legal prefix may precede the error.
    let mut error = None;
    for _ in 0..2 {
        match next(turn.as_mut()).await {
            Ok(Some(event)) => assert_eq!(
                event.get("type"),
                Some(&"turn_started".into()),
                "only the legal prefix may be published"
            ),
            Ok(None) => panic!("the stream settled without the inner rejection error"),
            Err(rejection) => {
                error = Some(rejection);
                break;
            }
        }
    }
    let error = error.expect("the inner rejection must fail the stream");
    assert!(
        error.message.contains("inner rejected input_accepted"),
        "{error:?}"
    );
    assert_eq!(next(turn.as_mut()).await.unwrap(), None);
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
    let session = session("live-cancel-pending", handler.clone()).await;
    let mut turn = session.submit(input(), options()).unwrap();
    assert_eq!(
        next(turn.as_mut()).await.unwrap().unwrap().get("type"),
        Some(&"turn_started".into())
    );
    session.abort(&"user-stop".into()).unwrap();
    let error = next(turn.as_mut()).await.unwrap_err();
    assert!(
        error.message.contains("terminal without proper lifecycle"),
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
    let session = session("live-cancel-pending-failure", handler.clone()).await;
    let mut turn = session.submit(input(), options()).unwrap();
    assert_eq!(
        next(turn.as_mut()).await.unwrap().unwrap().get("type"),
        Some(&"turn_started".into())
    );
    session.abort(&"user-stop".into()).unwrap();
    let error = next(turn.as_mut()).await.unwrap_err();
    assert!(
        error.message.contains("terminal without proper lifecycle"),
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
    let session = session("live-cancel-blocked-model-event", handler.clone()).await;
    let mut turn = session.submit(input(), options()).unwrap();
    assert_eq!(
        next(turn.as_mut()).await.unwrap().unwrap().get("type"),
        Some(&"turn_started".into())
    );
    session.abort(&"user-stop".into()).unwrap();
    let error = next(turn.as_mut()).await.unwrap_err();
    assert!(
        error.message.contains("terminal without proper lifecycle"),
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
    let session = session("live-cancel-queued-turn-failed", handler.clone()).await;
    let mut turn = session.submit(input(), options()).unwrap();
    for event_type in ["turn_started", "input_accepted"] {
        assert_eq!(
            next(turn.as_mut()).await.unwrap().unwrap().get("type"),
            Some(&event_type.into()),
            "{event_type}"
        );
    }
    session.abort(&"user-stop".into()).unwrap();
    let error = next(turn.as_mut()).await.unwrap_err();
    assert!(
        error.message.contains("terminal without proper lifecycle"),
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
    let session = session("live-cancel-slow-consumer", handler.clone()).await;
    let mut turn = session.submit(input(), options()).unwrap();
    assert_eq!(
        next(turn.as_mut()).await.unwrap().unwrap().get("type"),
        Some(&"turn_started".into())
    );
    session.abort(&"user-stop".into()).unwrap();
    // This next() is the first poll that observes the abort, so it arms the
    // cancel deadline; it delivers model_event and then the consumer stalls.
    assert_eq!(
        next(turn.as_mut()).await.unwrap().unwrap().get("type"),
        Some(&"model_event".into())
    );
    // Do not hard-code the grace: one second past the host's own constant.
    tokio::time::sleep(POST_CANCEL_EXIT_TIMEOUT + Duration::from_secs(1)).await;
    // The queued input_accepted and success final must still be delivered.
    assert_eq!(
        next(turn.as_mut()).await.unwrap().unwrap().get("type"),
        Some(&"input_accepted".into())
    );
    let completed = next(turn.as_mut()).await.unwrap().unwrap();
    assert_eq!(completed.get("type"), Some(&"turn_completed".into()));
    assert_eq!(
        completed
            .get("result")
            .and_then(|result| result.get("type")),
        Some(&"success".into())
    );
    assert_eq!(next(turn.as_mut()).await.unwrap(), None);
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
    let session = session("live-cancel-slow-consumer-failure", handler.clone()).await;
    let mut turn = session.submit(input(), options()).unwrap();
    assert_eq!(
        next(turn.as_mut()).await.unwrap().unwrap().get("type"),
        Some(&"turn_started".into())
    );
    session.abort(&"user-stop".into()).unwrap();
    // As in the success pairing, this next() arms the cancel deadline and
    // delivers model_event before the consumer stalls.
    assert_eq!(
        next(turn.as_mut()).await.unwrap().unwrap().get("type"),
        Some(&"model_event".into())
    );
    tokio::time::sleep(POST_CANCEL_EXIT_TIMEOUT + Duration::from_secs(1)).await;
    // The queued turn_failed and error final must still be delivered.
    assert_eq!(
        next(turn.as_mut()).await.unwrap().unwrap().get("type"),
        Some(&"turn_failed".into())
    );
    let completed = next(turn.as_mut()).await.unwrap().unwrap();
    assert_eq!(completed.get("type"), Some(&"turn_completed".into()));
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
    assert_eq!(next(turn.as_mut()).await.unwrap(), None);
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
    let session = session("live-cancel-slow-consumer", handler.clone()).await;
    let mut turn = session.submit(input(), options()).unwrap();
    assert_eq!(
        next(turn.as_mut()).await.unwrap().unwrap().get("type"),
        Some(&"turn_started".into())
    );
    session.abort(&"user-stop".into()).unwrap();
    assert_eq!(
        next(turn.as_mut()).await.unwrap().unwrap().get("type"),
        Some(&"model_event".into())
    );
    tokio::time::sleep(POST_CANCEL_EXIT_TIMEOUT + Duration::from_secs(1)).await;
    // The deadline is already past; a blocked callback must not extend it.
    // Half a fresh POST_CANCEL_EXIT_TIMEOUT is enough to prove the exit was
    // not re-armed.
    let error = timeout(POST_CANCEL_EXIT_TIMEOUT / 2, turn.next())
        .await
        .expect("the cancel exit must stay bounded with a blocked callback")
        .unwrap_err();
    assert!(
        error.message.contains("terminal without proper lifecycle"),
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
    let session = session("live-missing-accepted", handler.clone()).await;
    let mut turn = session.submit(input(), options()).unwrap();
    assert_eq!(
        next(turn.as_mut()).await.unwrap().unwrap().get("type"),
        Some(&"turn_started".into())
    );
    session.abort(&"user-stop".into()).unwrap();
    let error = next(turn.as_mut()).await.unwrap_err();
    assert!(
        error.message.contains("terminal without proper lifecycle"),
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
    let error = next(turn.as_mut()).await.unwrap_err();
    assert!(
        error.message.contains("terminal without proper lifecycle"),
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
    let error = next(turn.as_mut()).await.unwrap_err();
    assert!(
        error.message.contains("terminal without proper lifecycle"),
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
    let error = next(turn.as_mut()).await.unwrap_err();
    assert!(
        error.message.contains("terminal without proper lifecycle"),
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
    let error = next(turn.as_mut()).await.unwrap_err();
    assert!(error.message.contains("before turn_started"), "{error:?}");
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
    let session = session("cancel-pre-execute", handler.clone()).await;
    let mut turn = session.submit(input(), options()).unwrap();
    session.abort(&"pre-cancel".into()).unwrap();
    assert_aborted(&next(turn.as_mut()).await.unwrap().unwrap());
    assert_eq!(next(turn.as_mut()).await.unwrap(), None);
    assert_eq!(next(turn.as_mut()).await.unwrap(), None);
    assert!(handler.events.lock().unwrap().is_empty());
    assert_eq!(handler.disposed.load(Ordering::SeqCst), 1);
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
        input().get("messages"),
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
    // The second turn_started fails the receive hook, and the host cancels the
    // first event's in-flight application, so its publication races the
    // violation: only the violation error is guaranteed.
    let mut error = None;
    for _ in 0..2 {
        match next(turn.as_mut()).await {
            Ok(Some(event)) => assert_eq!(
                event.get("type"),
                Some(&"turn_started".into()),
                "only the legal prefix may be published"
            ),
            Ok(None) => panic!("the stream settled without the violation error"),
            Err(violation) => {
                error = Some(violation);
                break;
            }
        }
    }
    let error = error.expect("the violation must fail the stream");
    assert!(
        error
            .message
            .contains("turn_started after the turn already opened"),
        "{error:?}"
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
    let mut second = session.submit(input(), options()).unwrap();
    assert!(next(second.as_mut()).await.unwrap().is_some());
}

#[test]
fn execute_request_preserves_input_fields_and_option_identity() {
    let mut payload = input().as_object().unwrap().clone();
    payload.insert("provider".into(), "custom-provider".into());
    payload.insert("model".into(), "custom-model".into());
    payload.insert("turnId".into(), "payload-turn".into());
    payload.insert(
        "contextOverride".into(),
        parse_json(r#"{"messages":[],"custom":{"enabled":true}}"#).unwrap(),
    );
    let request = parse_execute_request(&JsValue::Object(payload.clone()), &options()).unwrap();
    assert_eq!(request.payload, payload);
    assert_eq!(request.identity.session_id, JsString::from("session"));
    assert_eq!(request.identity.turn_id, JsString::from("turn"));
    assert_eq!(request.identity.run_id, JsString::from("run"));
    assert_eq!(request.identity.operation_id, JsString::from("operation"));
    assert_eq!(request.identity.request_id, JsString::from("request"));
}

#[test]
fn execute_request_rejects_non_object_input() {
    for invalid in [
        JsValue::Undefined,
        JsValue::Null,
        JsValue::Bool(true),
        JsValue::Number(1.0),
        "message".into(),
        JsValue::Array(vec![]),
    ] {
        assert!(
            parse_execute_request(&invalid, &options())
                .unwrap_err()
                .message
                .contains("Expected execution input object")
        );
    }
}

#[tokio::test]
async fn sequential_turns_create_fresh_hosts_and_send_each_payload() {
    let handlers = Arc::new(Mutex::new(Vec::new()));
    let created = handlers.clone();
    let session = LiveSessionAdapter::new(move || {
        let created = created.clone();
        Box::pin(async move {
            let client = spawn_client("live-payload").await?;
            let handler = Arc::new(Handler::default());
            created.lock().unwrap().push(handler.clone());
            Ok((client, handler as Arc<dyn HostHandler>))
        })
    });
    for number in 1_u32..=2 {
        let turn_id = JsValue::String(format!("turn-{number}").into());
        let mut payload = input().as_object().unwrap().clone();
        payload.insert("custom".into(), JsValue::Number(f64::from(number)));
        payload.insert(
            "model".into(),
            JsValue::String(format!("model-{number}").into()),
        );
        let JsValue::Array(messages) = payload.get("messages").unwrap() else {
            panic!("test input must contain messages");
        };
        let mut messages = messages.clone();
        messages
            .push(parse_json(&format!(r#"{{"role":"user","content":"turn-{number}"}}"#)).unwrap());
        payload.insert("messages".into(), JsValue::Array(messages));
        let payload = JsValue::Object(payload);
        let mut turn_options = options().as_object().unwrap().clone();
        turn_options.insert("turnId".into(), turn_id.clone());
        let mut turn = session
            .submit(payload.clone(), JsValue::Object(turn_options))
            .unwrap();
        assert!(session.submit(input(), options()).is_err());
        let started = next(turn.as_mut()).await.unwrap().unwrap();
        assert_eq!(started.get("type"), Some(&"turn_started".into()));
        let received = next(turn.as_mut()).await.unwrap().unwrap();
        assert_eq!(received.get("receivedPayload"), Some(&payload));
        assert_eq!(received.get("turnId"), Some(&turn_id));
        let completed = next(turn.as_mut()).await.unwrap().unwrap();
        assert_eq!(completed.get("type"), Some(&"turn_completed".into()));
        let result = completed.get("result").unwrap();
        assert_eq!(result.get("type"), Some(&"success".into()));
        assert_eq!(result.get("turnId"), Some(&turn_id));
        assert_eq!(next(turn.as_mut()).await.unwrap(), None);
        let handlers = handlers.lock().unwrap();
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
    let session = LiveSessionAdapter::new(move || {
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
    let mut second = session.submit(input(), options()).unwrap();
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
    assert_aborted(&next(second.as_mut()).await.unwrap().unwrap());
    assert_eq!(next(second.as_mut()).await.unwrap(), None);
    assert_eq!(handler.disposed.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn dispose_cancels_pending_host_creation_and_rejects_future_submits() {
    let started = Arc::new(tokio::sync::Notify::new());
    let entered = started.clone();
    let session = LiveSessionAdapter::new(move || {
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
