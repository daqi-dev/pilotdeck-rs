use super::*;
use crate::host::Limits;
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
}

impl HostHandler for Handler {
    fn module_call(&self, _call: JsValue) -> HostFuture<JsObject> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Box::pin(async { Ok(JsObject::new()) })
    }

    fn event(&self, event: JsValue) -> HostFuture<()> {
        self.events.lock().unwrap().push(event);
        let fail = self.fail_event;
        Box::pin(async move {
            if fail {
                Err(HostError::new(HostErrorKind::Callback, "event failed"))
            } else {
                Ok(())
            }
        })
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

fn stream(execution: HostFuture<ExecuteResult>, events: mpsc::Receiver<JsValue>) -> LiveTurnStream {
    LiveTurnStream {
        execution: Some(execution),
        result: None,
        events,
        identity: parse_execute_request(&input(), &options())
            .unwrap()
            .identity,
        input: input(),
        integrity: StreamIntegrity::new(),
    }
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
    let handler = StreamHandler {
        inner: inner.clone(),
        events: tx,
    };
    let sent = expected.clone();
    let final_frame = wire(&terminal, true);
    let execution = Box::pin(async move {
        handler.module_call(JsValue::Null).await?;
        for event in sent {
            handler.event(wire(&event, false)).await?;
        }
        handler.event(final_frame.clone()).await?;
        handler.dispose().await?;
        Ok(ExecuteResult::Terminal(final_frame))
    });
    let mut turn = stream(execution, rx);
    // Isolate P1-2's terminal mapping from P1-1's explicitly deferred wiring.
    // The real HostClient test below keeps the production integrity state.
    for event in ["turn_started", "input_accepted", "turn_completed"] {
        turn.integrity.observe_event(event).unwrap();
    }
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
    let event = parse_json(r#"{"type":"input_accepted"}"#).unwrap();
    let sent = event.clone();
    let mut turn = stream(
        Box::pin(async move {
            tx.send(sent).await.unwrap();
            Err(HostError::new(HostErrorKind::Disconnected, "lost peer"))
        }),
        rx,
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
    let handler = StreamHandler {
        inner: Arc::new(Handler {
            fail_event: true,
            ..Handler::default()
        }),
        events: tx,
    };
    let mut turn = stream(
        Box::pin(async move {
            handler
                .event(wire(
                    &parse_json(r#"{"type":"input_accepted"}"#).unwrap(),
                    false,
                ))
                .await?;
            Ok(ExecuteResult::Aborted { terminal: None })
        }),
        rx,
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
    );
    let event = next(&mut turn).await.unwrap().unwrap();
    assert_eq!(event.get("result"), expected.get("result"));
    assert_eq!(event.get("messages"), expected.get("messages"));
    assert_eq!(next(&mut turn).await.unwrap(), None);
}

async fn session(mode: &str, handler: Arc<Handler>) -> LiveSessionAdapter {
    let mut command = Command::new("python3");
    command
        .arg(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../scripts/a6-host/fixture.py"
        ))
        .arg(mode);
    let client = HostClient::spawn(
        &mut command,
        Some(&"fixture".into()),
        Limits {
            handshake_timeout: Duration::from_secs(5),
            execution_timeout: Some(Duration::from_secs(10)),
            ..Limits::default()
        },
    )
    .await
    .unwrap();
    LiveSessionAdapter::new(client, handler)
}

#[tokio::test]
async fn host_stream_forwards_multiple_events_before_deferred_integrity_error() {
    let handler = Arc::new(Handler::default());
    let session = session("turn-completed", handler.clone()).await;
    let mut turn = session.submit(input(), options()).unwrap();
    for _ in 0..2 {
        let event = next(turn.as_mut()).await.unwrap().unwrap();
        assert_eq!(event.get("type"), Some(&"text_delta".into()));
        assert_eq!(event.get("text"), Some(&"hello".into()));
        assert!(event.get("payload").is_none());
    }
    // P1-1 is outside this patch: retain the existing fail-closed terminal gate.
    assert!(
        next(turn.as_mut())
            .await
            .unwrap_err()
            .message
            .contains("without proper completion")
    );
    assert_eq!(next(turn.as_mut()).await.unwrap(), None);
    assert_eq!(handler.events.lock().unwrap().len(), 3);
    assert_eq!(handler.disposed.load(Ordering::SeqCst), 1);
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
    let session = session("cancel-compaction", handler.clone()).await;
    let mut turn = session.submit(input(), options()).unwrap();
    let first = next(turn.as_mut()).await.unwrap().unwrap();
    assert_eq!(first.get("type"), Some(&"text_delta".into()));
    session
        .abort(&"compaction_persistence_failed".into())
        .unwrap();
    assert_aborted(&next(turn.as_mut()).await.unwrap().unwrap());
    assert_eq!(next(turn.as_mut()).await.unwrap(), None);
    assert_eq!(handler.events.lock().unwrap().len(), 1);
    assert_eq!(handler.disposed.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn dropping_an_active_stream_disposes_its_host_handler() {
    let handler = Arc::new(Handler::default());
    let session = session("positive", handler.clone()).await;
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
}
