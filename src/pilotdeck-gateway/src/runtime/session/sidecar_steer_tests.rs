use super::*;
use pilotdeck_protocol::parse_json;

#[derive(Default)]
struct Persistence {
    entries: Mutex<Vec<JsValue>>,
    fail_message: AtomicBool,
}
impl SteerPersistence for Persistence {
    fn mutation(&self, _: JsString, mutation: JsObject) -> HostFuture<()> {
        self.entries.lock().unwrap().push(JsValue::Object(mutation));
        Box::pin(async { Ok(()) })
    }
    fn message(&self, _: JsString, message: JsValue) -> HostFuture<()> {
        let failed = self.fail_message.load(Ordering::SeqCst);
        if !failed {
            self.entries.lock().unwrap().push(message);
        }
        Box::pin(async move {
            if failed {
                Err(ModuleError::new("write failed").into())
            } else {
                Ok(())
            }
        })
    }
}
struct NoSpawn;
impl SidecarSpawner for NoSpawn {
    fn requires_subprocess(&self) -> bool {
        true
    }

    fn command(&self) -> std::io::Result<tokio::process::Command> {
        Ok(tokio::process::Command::new("false"))
    }
}
fn json(input: &str) -> JsValue {
    parse_json(input).unwrap()
}
fn fixture(
    persistence: Arc<Persistence>,
) -> (
    SidecarSession,
    ModuleHost,
    mpsc::UnboundedReceiver<Option<Result<JsValue, RuntimeError>>>,
) {
    let session = SidecarSession::with_steer_persistence(
        SidecarSessionConfig {
            limits: Limits::default(),
            session_key: "s".into(),
            project_key: None,
            cwd: "/tmp".into(),
            snapshot: JsValue::Null,
            services: None,
        },
        Arc::new(NoSpawn),
        persistence.clone(),
    );
    let (sink, receiver) = mpsc::unbounded_channel();
    session.install_sink(sink.clone(), Cancellation::default());
    let steers = session
        .active
        .lock()
        .unwrap()
        .as_ref()
        .unwrap()
        .steers
        .clone();
    let events = Arc::new(super::super::module_host::EventBuffer::streaming(Arc::new(
        move |event| {
            let _ = sink.send(Some(Ok(event)));
            Ok(())
        },
    )));
    let router = super::super::module_host::build_with_steers(
        JsValue::Null,
        &json(r#"{"sessionId":"s","turnId":"test-turn","agent":{"cwd":"/tmp"}}"#),
        events.clone(),
        crate::runtime::network::Cancellation::default(),
        steers.clone(),
    )
    .unwrap();
    (
        session,
        ModuleHost {
            live: None,
            router,
            events,
            steers,
            persistence: Some(persistence),
            transcript: None,
            cancellation: Cancellation::default(),
            run_id: None,
            session_id: "s".into(),
            turn_id: "test-turn".into(),
        },
        receiver,
    )
}
fn steer() -> SteerMessage {
    SteerMessage {
        item_id: "i".into(),
        message: json(
            r#"{"role":"user","content":[{"type":"text","text":"change direction"}],"metadata":{"purpose":"mid_turn_steer","queueItemId":"i"}}"#,
        ),
        allowed_read_files: Some(vec!["/tmp/registered.txt".into()]),
    }
}

#[test]
fn rejected_module_terminal_is_persisted_once_with_ts_failure_details() {
    use crate::storage::{AsyncTranscriptAdapter, JsonlTranscriptStore, SessionStorageInput};
    let (_, mut host, _) = fixture(Arc::new(Persistence::default()));
    let root =
        std::env::temp_dir().join(format!("pdrs-rejected-terminal-{}", uuid::Uuid::new_v4()));
    let chat = root.join("chat");
    let adapter = AsyncTranscriptAdapter::new(
        Arc::new(JsonlTranscriptStore::new()),
        Arc::new(move |_| chat.clone()),
    );
    let storage = adapter.create_session_storage(SessionStorageInput {
        project_root: root.clone(),
        pilot_home: root,
        session_id: "s".into(),
        now: None,
    })
    .unwrap();
    let writer = Arc::new(storage.transcript);
    host.transcript = Some(writer.clone());
    for (index, outcome, code, kind, stop_reason) in [
        (
            0,
            "failed",
            "DEADLINE_EXCEEDED",
            "error",
            "aborted_streaming",
        ),
        (1, "failed", "MODULE_FAILURE", "error", "model_error"),
        (2, "cancelled", "MODULE_CANCELLED", "aborted", "aborted"),
    ] {
        host.turn_id = format!("turn-{index}").as_str().into();
        let failure = crate::metadata::object([
            ("code", code.into()),
            ("message", "the actual rejection".into()),
        ]);
        let rejection = crate::metadata::object([
            ("kind", "response".into()),
            ("final", JsValue::Bool(true)),
            ("outcome", outcome.into()),
            ("code", code.into()),
            ("error", failure.clone()),
        ]);
        let terminal = host.finish_rejected(rejection.clone(), false).unwrap();
        assert_eq!(terminal.get("type"), Some(&"turn_completed".into()));
        let result = terminal.get("result").unwrap();
        assert_eq!(result.get("type"), Some(&kind.into()));
        assert_eq!(result.get("stopReason"), Some(&stop_reason.into()));
        if outcome == "cancelled" {
            assert_eq!(result.get("error"), Some(&rejection));
        } else {
            let JsValue::Array(errors) = result.get("errors").unwrap() else {
                panic!("a rejected execute must have agent errors");
            };
            assert_eq!(errors.len(), 1);
            assert_eq!(
                errors[0].get("code"),
                Some(&"agent_execution_rejected".into())
            );
            assert_eq!(
                errors[0].get("message"),
                Some(&"the actual rejection".into())
            );
            assert_eq!(
                errors[0].get("details").unwrap().get("sidecarCode"),
                Some(&code.into())
            );
            assert_eq!(
                errors[0].get("details").unwrap().get("sidecarError"),
                Some(&failure)
            );
            assert_eq!(result.get("turns"), Some(&JsValue::Number(0.0)));
            assert_eq!(result.get("startedAt"), result.get("completedAt"));
        }
    }
    let entries = crate::storage::read_transcript(writer.path())
        .unwrap()
        .entries;
    let results: Vec<_> = entries
        .iter()
        .filter(|entry| entry.get("type") == Some(&"turn_result".into()))
        .collect();
    assert_eq!(results.len(), 3);
    for (index, kind) in ["error", "error", "aborted"].into_iter().enumerate() {
        assert_eq!(
            results[index].get("result").unwrap().get("type"),
            Some(&kind.into())
        );
    }
}

#[test]
fn bare_transport_rejection_is_preserved_and_fork_projects_one_agent_terminal() {
    let (_, host, _) = fixture(Arc::new(Persistence::default()));
    let rejection =
        json(r#"{"kind":"response","final":true,"outcome":"failed","code":"CAPTURED"}"#);
    assert_eq!(
        host.finish_rejected(rejection.clone(), false).unwrap(),
        rejection
    );
    let terminal = host.finish_rejected(rejection, true).unwrap();
    assert_eq!(terminal.get("type"), Some(&"turn_completed".into()));
    let result = terminal.get("result").unwrap();
    assert_eq!(result.get("stopReason"), Some(&"model_error".into()));
    let JsValue::Array(errors) = result.get("errors").unwrap() else {
        panic!("a rejected execute must have agent errors");
    };
    assert_eq!(
        errors[0].get("message"),
        Some(&"Sidecar rejected execute request: CAPTURED".into())
    );
    let existing =
        json(r#"{"payload":{"result":{"type":"error","stopReason":"actual_agent_result"}}}"#);
    let terminal = host.finish_rejected(existing.clone(), false).unwrap();
    assert_eq!(
        terminal.get("result"),
        existing.get("payload").unwrap().get("result")
    );
}

#[test]
fn unknown_module_terminal_preserves_protocol_fact_without_a_known_turn_result() {
    let (_, mut host, _) = fixture(Arc::new(Persistence::default()));
    host.run_id = Some("deadline-run".into());
    host.cancellation
        .cancel(Some("timeout:deadline-run".into()));
    let payload = json(
        r#"{"kind":"event","final":true,"streamId":"deadline-stream","outcome":"result_unknown","code":"DEADLINE_EXCEEDED","payload":{"error":{"code":"DEADLINE_EXCEEDED","message":"Module execution deadline exceeded."}},"error":{"code":"DEADLINE_EXCEEDED","message":"Module execution deadline exceeded."}}"#,
    );
    let error = host.finish_terminal(payload.clone()).unwrap_err();
    assert_eq!(error.kind, crate::host::HostErrorKind::ResultUnknown);
    assert!(error.accepted);
    assert_eq!(error.stream_id, Some("deadline-stream".into()));
    assert_eq!(
        error.message,
        "Sidecar terminal outcome is result_unknown; host reconciliation is required."
    );
    let detail = error.module_error.unwrap();
    assert_eq!(detail.code, Some("AGENT_LOOP_RESULT_UNKNOWN".into()));
    assert_eq!(detail.canonical, payload.as_object().cloned());
    let error = host.finish_terminal(payload.clone()).unwrap_err();
    let failure = ModuleHost::runtime_failure(error);
    assert_eq!(failure.causes.len(), 1);
    assert_eq!(parse_json(&failure.causes[0].message).unwrap(), payload);
}
#[test]
fn unknown_without_matching_gateway_timeout_becomes_ts_error_result() {
    for reason in [None, Some("user_stop"), Some("timeout:other-run")] {
        let (_, mut host, _) = fixture(Arc::new(Persistence::default()));
        host.run_id = Some("this-run".into());
        if let Some(reason) = reason {
            host.cancellation.cancel(Some(reason.into()));
        }
        let terminal = host
            .finish_terminal(json(
                r#"{"outcome":"result_unknown","code":"DEADLINE_EXCEEDED"}"#,
            ))
            .unwrap();
        let result = terminal.get("result").unwrap();
        assert_eq!(result.get("type"), Some(&"error".into()));
        assert_eq!(result.get("stopReason"), Some(&"model_error".into()));
        let JsValue::Array(errors) = result.get("errors").unwrap() else {
            panic!("error result")
        };
        assert_eq!(errors[0].get("code"), Some(&"agent_invalid_state".into()));
        assert_eq!(result.get("startedAt"), result.get("completedAt"));
    }
}

fn applied(message: &JsValue) -> JsValue {
    let mut event = JsObject::new();
    event.insert("type".into(), "steer_applied".into());
    event.insert("sessionId".into(), "s".into());
    event.insert("turnId".into(), "test-turn".into());
    event.insert("itemId".into(), "i".into());
    event.insert("message".into(), message.clone());
    let mut frame = JsObject::new();
    frame.insert("final".into(), JsValue::Bool(false));
    frame.insert("payload".into(), JsValue::Object(event));
    JsValue::Object(frame)
}

#[tokio::test]
async fn live_sidecar_controls_reach_the_real_module_router_and_apply_after_writing() {
    let persistence = Arc::new(Persistence::default());
    let (session, host, mut receiver) = fixture(persistence.clone());
    let message = steer();
    assert!(
        session
            .steer("test-turn".into(), message.clone())
            .await
            .unwrap()
            .accepted
    );
    let response = host
        .module_call(json(
            r#"{"module":"turn","payload":{"operation":"drain_steer"}}"#,
        ))
        .await
        .unwrap();
    assert_eq!(
        response.get("messages"),
        Some(&JsValue::Array(vec![JsValue::Object(message.to_object())]))
    );
    host.event(applied(&message.message)).await.unwrap();
    assert_eq!(host.steers.messages(), vec![message.message.clone()]);
    assert!(host.steers.unacknowledged().is_empty());
    assert!(
        matches!(receiver.try_recv().unwrap(), Some(Ok(event)) if event.get("type") == Some(&"steer_applied".into()))
    );
    assert_eq!(
        persistence.entries.lock().unwrap().len(),
        3,
        "insert, claim, durable message"
    );
    assert_eq!(
        session
            .cancel_steer("test-turn".into(), "i".into())
            .await
            .unwrap()
            .reason,
        Some(SteerReason::TooLate)
    );
}

#[tokio::test]
async fn failed_message_write_cannot_ack_or_emit_applied_steer() {
    let persistence = Arc::new(Persistence::default());
    let (session, host, mut receiver) = fixture(persistence.clone());
    let message = steer();
    session
        .steer("test-turn".into(), message.clone())
        .await
        .unwrap();
    host.module_call(json(
        r#"{"module":"turn","payload":{"operation":"drain_steer"}}"#,
    ))
    .await
    .unwrap();
    persistence.fail_message.store(true, Ordering::SeqCst);
    assert!(host.event(applied(&message.message)).await.is_err());
    assert!(host.steers.messages().is_empty());
    assert_eq!(host.steers.unacknowledged().len(), 1);
    assert!(receiver.try_recv().is_err());
}

#[tokio::test]
async fn stale_completion_cannot_clear_new_active_turn_or_deliver_to_it() {
    let (session, _host, mut receiver) = fixture(Arc::new(Persistence::default()));
    let old = session
        .active
        .lock()
        .unwrap()
        .as_ref()
        .unwrap()
        .identity
        .clone();
    let steers = session
        .active
        .lock()
        .unwrap()
        .as_ref()
        .unwrap()
        .steers
        .clone();
    let sink = session
        .active
        .lock()
        .unwrap()
        .as_ref()
        .unwrap()
        .sink
        .clone();
    let guard = ActiveGuard {
        active: session.active.clone(),
        identity: old.clone(),
        sink,
        steers,
        turn_id: "test-turn".into(),
        completed: watch::channel(false).0,
        hook_binding: None,
        hook_subscription: Vec::new(),
    };
    let (next_sink, _next_receiver) = mpsc::unbounded_channel();
    session.install_sink(next_sink, Cancellation::default());
    assert!(!is_current(&session.active, &old));
    drop(guard);
    assert!(session.active.lock().unwrap().is_some());
    assert!(receiver.try_recv().unwrap().is_none());
}

#[tokio::test]
async fn abort_revokes_controls_without_fabricating_acceptance() {
    let (session, _, _) = fixture(Arc::new(Persistence::default()));
    session.abort(&"user_stop".into()).unwrap();
    assert_eq!(
        session
            .steer("test-turn".into(), steer())
            .await
            .unwrap()
            .reason,
        Some(SteerReason::NoActiveTurn)
    );
    assert_eq!(
        session
            .cancel_steer("test-turn".into(), "i".into())
            .await
            .unwrap()
            .reason,
        Some(SteerReason::NoActiveTurn)
    );
}

#[tokio::test]
async fn abort_completion_waits_for_worker_cleanup_before_the_next_submit() {
    use std::{future::Future, task::Poll};
    let (session, _host, receiver) = fixture(Arc::new(Persistence::default()));
    let (completed, completion) = watch::channel(false);
    let guard = {
        let mut active = session.active.lock().unwrap();
        let turn = active.as_mut().unwrap();
        turn.completion = completion.clone();
        ActiveGuard {
            active: session.active.clone(),
            identity: turn.identity.clone(),
            sink: turn.sink.clone(),
            steers: turn.steers.clone(),
            turn_id: "test-turn".into(),
            completed,
            hook_binding: None,
            hook_subscription: Vec::new(),
        }
    };
    let mut stream = SidecarTurn {
        receiver,
        completion,
        ended: false,
    };
    session.abort(&"user_stop".into()).unwrap();
    let mut finish = Box::pin(stream.next());
    std::future::poll_fn(|context| {
        assert!(
            matches!(finish.as_mut().poll(context), Poll::Pending),
            "the synthetic None cannot release the live handle before cleanup"
        );
        Poll::Ready(())
    })
    .await;
    let input = json(r#"{"agent":{"cwd":"/tmp","provider":"test","model":"m"},"messages":[]}"#);
    let options = json(r#"{"turnId":"next"}"#);
    assert!(session.submit(input.clone(), options.clone()).is_err());
    drop(guard);
    assert!(finish.await.unwrap().is_none());
    assert!(session.active.lock().unwrap().is_none());
    let mut next = session
        .submit(input, options)
        .expect("the next submit is admitted immediately after the completion fence");
    // NoSpawn intentionally fails the external sidecar handshake. Drain that
    // real worker failure to prove its cleanup also releases the next slot.
    assert!(next.next().await.is_err());
    assert!(session.active.lock().unwrap().is_none());
}

#[derive(Default)]
struct RecordStore {
    entries: Mutex<Vec<JsValue>>,
    fail_next: AtomicBool,
}
impl crate::storage::TranscriptStore for RecordStore {
    fn append(
        &self,
        _: &crate::storage::TranscriptKey,
        entry: &JsValue,
    ) -> crate::storage::StoreResult<()> {
        if self.fail_next.swap(false, Ordering::SeqCst) {
            return Err(crate::storage::LedgerError::Invalid(
                "injected write failure".into(),
            ));
        }
        self.entries.lock().unwrap().push(entry.clone());
        Ok(())
    }
    fn read(
        &self,
        _: &crate::storage::TranscriptKey,
        _: Option<u64>,
    ) -> crate::storage::StoreResult<crate::storage::TranscriptRead> {
        Ok(crate::storage::TranscriptRead {
            entries: self.entries.lock().unwrap().clone(),
            diagnostics: Vec::new(),
        })
    }
}
fn native_storage(store: Arc<RecordStore>) -> crate::storage::SessionStorage {
    let adapter = crate::storage::AsyncTranscriptAdapter::new(
        store,
        Arc::new(|_| std::path::PathBuf::from("/pdrs-in-memory/chat")),
    );
    adapter
        .create_session_storage(crate::storage::SessionStorageInput {
            project_root: "/pdrs-in-memory/project".into(),
            pilot_home: "/pdrs-in-memory/home".into(),
            session_id: "s".into(),
            now: Some(Arc::new(|| "2026-10-27T00:00:00.000Z".into())),
        })
        .unwrap()
}
fn native_write(
    writer: &TranscriptWriter,
    turn: &str,
    draft: &str,
) -> crate::storage::AdapterResult<()> {
    writer.record_native_session_event(
        &"s".into(),
        &turn.into(),
        json(draft).as_object().unwrap().clone(),
    )
}
fn assert_domain(result: crate::storage::AdapterResult<()>, expected: &str) {
    assert!(
        matches!(
            result,
            Err(
                crate::storage::AdapterError::Validation { code, .. }
                    | crate::storage::AdapterError::Domain { code, .. }
            ) if code == expected
        )
    );
}

#[test]
fn native_inbox_requires_a_live_turn_claim_before_message_and_empty_queue_before_result() {
    let store = Arc::new(RecordStore::default());
    let storage = native_storage(store.clone());
    let writer = &storage.transcript;
    let insert = r#"{"type":"inbox_mutation","mutation":"insert","itemId":"i","message":{}}"#;
    assert_domain(native_write(writer, "t", insert), "event_outside_turn");
    assert!(store.entries.lock().unwrap().is_empty());
    native_write(writer, "t", r#"{"type":"turn_started"}"#).unwrap();
    native_write(writer, "t", insert).unwrap();
    assert_domain(native_write(writer, "t", insert), "invalid_inbox_mutation");
    assert_domain(
        native_write(
            writer,
            "t",
            r#"{"type":"inbox_mutation","mutation":"claim","itemId":"absent"}"#,
        ),
        "invalid_inbox_mutation",
    );
    let durable = r#"{"type":"durable_message","message":{"role":"user","content":[{"type":"text","text":"steer"}],"metadata":{"queueItemId":"i"}}}"#;
    assert_domain(native_write(writer, "t", durable), "invalid_inbox_mutation");
    let terminal =
        r#"{"type":"turn_result","result":{"type":"success","usage":{},"permissionDenials":[]}}"#;
    assert_domain(
        native_write(writer, "t", terminal),
        "turn_has_pending_inbox",
    );
    native_write(
        writer,
        "t",
        r#"{"type":"inbox_mutation","mutation":"claim","itemId":"i"}"#,
    )
    .unwrap();
    assert_domain(
        native_write(
            writer,
            "t",
            r#"{"type":"inbox_mutation","mutation":"cancel","itemId":"i"}"#,
        ),
        "invalid_inbox_mutation",
    );
    native_write(writer, "t", durable).unwrap();
    native_write(writer, "t", terminal).unwrap();
    assert_domain(native_write(writer, "t", insert), "event_outside_turn");
    assert_domain(native_write(writer, "t", durable), "invalid_inbox_mutation");
    assert_domain(native_write(writer, "t", terminal), "event_outside_turn");
    // daqi validates this shape natively instead of refusing it as a Node-only
    // domain, so the same entry now fails domain validation by name.
    assert!(matches!(
        writer.record_session_event(
            &"s".into(),
            &"new".into(),
            json(insert).as_object().unwrap().clone()
        ),
        Err(crate::storage::AdapterError::Domain { .. })
    ));
    let restored = native_storage(store);
    restored.restore().unwrap();
    assert_domain(
        native_write(&restored.transcript, "t", insert),
        "event_outside_turn",
    );
}

#[test]
fn failed_native_inbox_writes_roll_back_sequence_and_transition_state() {
    let store = Arc::new(RecordStore::default());
    let storage = native_storage(store.clone());
    let writer = &storage.transcript;
    native_write(writer, "t", r#"{"type":"turn_started"}"#).unwrap();
    let insert = r#"{"type":"inbox_mutation","mutation":"insert","itemId":"i","message":{}}"#;
    let before = writer.snapshot_state();
    store.fail_next.store(true, Ordering::SeqCst);
    assert!(matches!(
        native_write(writer, "t", insert),
        Err(crate::storage::AdapterError::Store(_))
    ));
    assert_eq!(writer.snapshot_state(), before);
    native_write(writer, "t", insert).unwrap();
    let claim = r#"{"type":"inbox_mutation","mutation":"claim","itemId":"i"}"#;
    store.fail_next.store(true, Ordering::SeqCst);
    assert!(native_write(writer, "t", claim).is_err());
    native_write(writer, "t", claim).unwrap();
    let durable = r#"{"type":"durable_message","message":{"role":"user","content":[],"metadata":{"queueItemId":"i"}}}"#;
    store.fail_next.store(true, Ordering::SeqCst);
    assert!(native_write(writer, "t", durable).is_err());
    native_write(writer, "t", durable).unwrap();
    assert_eq!(writer.snapshot_state().sequence, 4);
    assert_eq!(store.entries.lock().unwrap().len(), 4);
}

#[test]
fn native_inbox_cancellation_discard_and_unknown_mutation_match_ts() {
    let store = Arc::new(RecordStore::default());
    let storage = native_storage(store);
    let writer = &storage.transcript;
    native_write(writer, "t", r#"{"type":"turn_started"}"#).unwrap();
    native_write(
        writer,
        "t",
        r#"{"type":"inbox_mutation","mutation":"cancel","itemId":"cancelled"}"#,
    )
    .unwrap();
    native_write(
        writer,
        "t",
        r#"{"type":"inbox_mutation","mutation":"cancel","itemId":"cancelled"}"#,
    )
    .unwrap();
    assert_domain(
        native_write(
            writer,
            "t",
            r#"{"type":"inbox_mutation","mutation":"insert","itemId":"cancelled","message":{}}"#,
        ),
        "invalid_inbox_mutation",
    );
    assert_domain(
        native_write(
            writer,
            "t",
            r#"{"type":"inbox_mutation","mutation":"discard","itemId":"cancelled"}"#,
        ),
        "invalid_inbox_mutation",
    );
    native_write(
        writer,
        "t",
        r#"{"type":"inbox_mutation","mutation":"unknown","itemId":"unmodified"}"#,
    )
    .unwrap();
    native_write(
        writer,
        "t",
        r#"{"type":"inbox_mutation","mutation":"insert","itemId":"unmodified","message":{}}"#,
    )
    .unwrap();
    native_write(
        writer,
        "t",
        r#"{"type":"inbox_mutation","mutation":"discard","itemId":"unmodified"}"#,
    )
    .unwrap();
    assert_domain(
        native_write(
            writer,
            "t",
            r#"{"type":"inbox_mutation","mutation":"cancel","itemId":""}"#,
        ),
        "invalid_inbox_mutation",
    );
    native_write(
        writer,
        "t",
        r#"{"type":"turn_result","result":{"usage":{},"permissionDenials":[]}}"#,
    )
    .unwrap();
}
