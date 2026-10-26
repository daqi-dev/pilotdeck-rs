use super::*;
use crate::runtime::project::{ProjectRuntimeRegistry, ResourceBundle};
use crate::storage::{
    JsonlTranscriptStore, StoreResult, TranscriptKey, TranscriptRead, TranscriptStore,
};

fn rows() -> Vec<JsValue> {
    static ORACLE: std::sync::OnceLock<JsValue> = std::sync::OnceLock::new();
    values(
        ORACLE
            .get_or_init(|| {
                crate::test_baseline::oracle(include_str!(
                    "../../../tests/fixtures/turn-runner/oracle.json"
                ))
            })
            .get("forward")
            .unwrap(),
    )
}
fn values(value: &JsValue) -> Vec<JsValue> {
    let JsValue::Array(values) = value else {
        panic!("array")
    };
    values.clone()
}
// Only wall-clock fallback timestamps differ; writer IDs/timestamps are fixed.
fn clean(value: &mut JsValue) {
    let JsValue::Array(events) = value else {
        panic!("event list")
    };
    for event in events {
        let Some(fields) = event.as_object() else {
            continue;
        };
        let terminal = fields.get("type") == Some(&"turn_completed".into());
        let durable = fields.get("type") == Some(&"turn_result".into());
        if !terminal && !durable {
            continue;
        }
        let mut out = JsObject::new();
        for (key, value) in fields.entries() {
            if terminal && key.equals_str("messages") {
                continue;
            }
            let mut value = value.clone();
            if key.equals_str("result") && value.get("turns") == Some(&JsValue::Number(0.0)) {
                let JsValue::Object(ref mut result) = value else {
                    panic!("result")
                };
                for key in ["startedAt", "completedAt"] {
                    result.insert(key.into(), "CLOCK".into());
                }
            }
            out.insert(key.clone(), value);
        }
        *event = JsValue::Object(out);
    }
}
struct OrderedStore(Arc<RecordingStore>, Arc<Handler>);
impl TranscriptStore for OrderedStore {
    fn append(&self, key: &TranscriptKey, entry: &JsValue) -> StoreResult<()> {
        if entry.get("type") == Some(&"turn_result".into()) {
            assert_eq!(
                self.1.disposed.load(Ordering::SeqCst),
                1,
                "loop cleanup before catch complete"
            );
        }
        self.0.append(key, entry)
    }
    fn read(&self, key: &TranscriptKey, limit: Option<u64>) -> StoreResult<TranscriptRead> {
        self.0.read(key, limit)
    }
}
struct Fixture {
    root: std::path::PathBuf,
    key: TranscriptKey,
    store: Arc<RecordingStore>,
    registry: Arc<ProjectRuntimeRegistry>,
    session: Arc<Mutex<SessionState>>,
    turn: LiveTurnStream,
}
impl Fixture {
    fn new(row: &JsValue) -> Self {
        let root = std::env::temp_dir().join(format!("pdrs-forward-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&root).unwrap();
        let key = TranscriptKey {
            project_root: root.clone(),
            pilot_home: root.join("home"),
            session_id: "s".into(),
            transcript_path: root.join("s.jsonl"),
        };
        let registry = Arc::new(ProjectRuntimeRegistry::new(
            &root,
            Arc::new(|_: &std::path::Path, _: &mut ResourceBundle| Ok(())),
        ));
        let kind = match row.get("fault").and_then(JsValue::as_string) {
            Some(v) if v.equals_str("agent_status_message") => "agent_status_message",
            Some(v) if v.equals_str("turn_result") => "turn_result",
            _ => "",
        };
        let store = Arc::new(RecordingStore {
            disk: JsonlTranscriptStore::new(),
            kind,
            fail: usize::from(!kind.is_empty() && row.get("noFail") != Some(&JsValue::Bool(true))),
            completes: AtomicUsize::new(0),
            attempts: AtomicUsize::new(0),
            gate: Mutex::new(None),
        });
        let (inner, mut handler, rx) = buffered_handler(1);
        Arc::get_mut(&mut handler).unwrap().callback_cancel = Some(Cancellation::default());
        let checked = Arc::new(OrderedStore(store.clone(), inner.clone()));
        let writer = Recorder::new(checked, key.clone()).writer().clone();
        let ids = AtomicUsize::new(0);
        *writer.test_sources.lock().unwrap() = Some((
            Arc::new(|| Ok("2026-10-26T00:00:00.000Z".into())),
            Arc::new(move || Ok(format!("id-{}", ids.fetch_add(1, Ordering::SeqCst) + 1))),
        ));
        let recorder = Arc::new(Recorder::from_writer(writer));
        let session = Arc::new(Mutex::new(SessionState {
            disposed: false,
            active: Some(Cancellation::default()),
        }));
        let active = Arc::new(ActiveTurn(session.clone()));
        let integrity = handler.integrity.clone();
        let callback_cancel = handler.callback_cancel.clone();
        let inputs = values(row.get("inputs").unwrap());
        let result = row.get("result").unwrap().clone();
        let open_step = row.get("name") == Some(&"catch_invalid_step".into());
        let pump = row.get("source") == Some(&"pump".into());
        let host_wait = row.get("source") == Some(&"host".into());
        let record = recorder.clone();
        let (ready, execute) = oneshot::channel();
        let mut turn = stream(
            Box::pin(async move {
                execute.await.unwrap();
                let outcome = async {
                    for kind in ["turn_started", "input_accepted"] {
                        deliver(
                            &handler,
                            &crate::metadata::object([
                                ("type", kind.into()),
                                ("sessionId", "s".into()),
                                ("turnId", "t".into()),
                            ]),
                            false,
                        )
                        .await?;
                    }
                    if open_step {
                        record
                            .record_model_request(
                                &"s".into(),
                                &"t".into(),
                                crate::metadata::object([("messages", JsValue::Array(vec![]))]),
                            )
                            .unwrap();
                    }
                    for item in inputs {
                        let event = item.get("event").unwrap();
                        if item.get("wire") == Some(&JsValue::Bool(true)) {
                            deliver(&handler, event, false).await?;
                        } else {
                            queue_host_events(&inner, std::slice::from_ref(event));
                            if pump {
                                let cancel = handler.callback_cancel.clone().unwrap();
                                let owner = handler.clone();
                                return handler
                                    .with_pump(
                                        async move {
                                            cancel.cancelled().await;
                                            owner.dispose().await?;
                                            Ok(ExecuteResult::Aborted { terminal: None })
                                        },
                                        parse_execute_request(&input(), &options(), vec![])
                                            .unwrap()
                                            .0
                                            .identity,
                                    )
                                    .await;
                            }
                            if handler.module_call(JsValue::Null).await.is_err() || host_wait {
                                // Real HostClient sends a module error response, then observes cancel.
                                handler.callback_cancel.as_ref().unwrap().cancelled().await;
                                return Ok(ExecuteResult::Aborted { terminal: None });
                            }
                        }
                    }
                    Ok(ExecuteResult::Terminal(wire(
                        &crate::metadata::object([
                            ("result", result),
                            ("messages", JsValue::Array(vec![])),
                        ]),
                        true,
                    )))
                }
                .await;
                if inner.disposed.load(Ordering::SeqCst) == 0 {
                    handler.dispose().await?;
                }
                outcome
            }),
            rx,
            integrity,
        );
        turn.input = TurnInput::from_agent_input(
            &parse_json(r#"{"type":"text","text":"probe"}"#).unwrap(),
            &JsValue::Null,
            vec![],
            vec![],
        )
        .unwrap();
        turn.recording = Some(Recording::new(
            TurnRunner::new(
                recorder,
                Arc::new(registry.acquire(None).unwrap()),
                "s".into(),
                "t".into(),
            ),
            active.clone(),
        ));
        turn.identity.session_id = "s".into();
        turn.identity.turn_id = "t".into();
        turn.active = Some(active);
        turn.execute_ready = Some(ready);
        turn.callback_cancel = callback_cancel;
        Self {
            root,
            key,
            store,
            registry,
            session,
            turn,
        }
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.root).unwrap();
    }
}
async fn matrix(name: &str) {
    let row = rows()
        .into_iter()
        .find(|r| r.get("name") == Some(&name.into()))
        .unwrap();
    let mut f = Fixture::new(&row);
    let mut actual = Vec::new();
    while let Some(event) = next(&mut f.turn).await.unwrap() {
        actual.push(event);
    }
    let mut expected = values(row.get("events").unwrap());
    assert_eq!(
        expected.remove(2).get("type"),
        Some(&"user_prompt_submitted".into())
    );
    let mut actual = JsValue::Array(actual);
    let mut expected = JsValue::Array(expected);
    clean(&mut actual);
    clean(&mut expected);
    assert_eq!(actual, expected, "forward event order/payload");
    let bytes = std::fs::read_to_string(&f.key.transcript_path).unwrap();
    let expected = row
        .get("transcript")
        .unwrap()
        .as_string()
        .unwrap()
        .to_utf8()
        .unwrap();
    let parse = |s: &str| {
        let mut value = JsValue::Array(s.lines().map(|line| parse_json(line).unwrap()).collect());
        clean(&mut value);
        value
    };
    assert_eq!(
        parse(&bytes),
        parse(&expected),
        "forward durable callback/complete"
    );
    assert!(
        f.session.lock().unwrap().active.is_none(),
        "forward actual slot released"
    );
    assert_eq!(
        f.registry.resolve(None).unwrap().lease_count(),
        0,
        "forward lease released"
    );
}
macro_rules! case {
    ($test:ident,) => {
        #[tokio::test]
        async fn $test() {
            matrix(stringify!($test).strip_prefix("forward_").unwrap()).await;
        }
    };
}
case!(forward_event_session_started,);
case!(forward_event_session_ended,);
case!(forward_event_user_prompt_submitted,);
case!(forward_event_setup_completed,);
case!(forward_event_model_request_started,);
case!(forward_event_model_event,);
case!(forward_event_instructions_loaded,);
case!(forward_event_assistant_message,);
case!(forward_event_prompt_suggestion,);
case!(forward_event_steer_applied,);
case!(forward_event_steer_unapplied,);
case!(forward_event_tool_calls_detected,);
case!(forward_event_pre_tool_execute,);
case!(forward_event_post_tool_execute,);
case!(forward_event_tool_progress,);
case!(forward_event_permission_requested,);
case!(forward_event_permission_denied,);
case!(forward_event_tool_result,);
case!(forward_event_tool_results_projected,);
case!(forward_event_mode_change_requested,);
case!(forward_event_stop_requested,);
case!(forward_event_stop_failure,);
case!(forward_event_compact_started,);
case!(forward_event_compact_completed,);
case!(forward_event_context_budget,);
case!(forward_event_warning,);
case!(forward_event_token_cap_adjusted,);
case!(forward_event_empty_output_recovery,);
case!(forward_event_model_recovery_failed,);
case!(forward_event_subagent_started,);
case!(forward_event_subagent_completed,);
case!(forward_event_observer_report,);
case!(forward_event_subagent_status,);
case!(forward_event_subagent_model_event,);
case!(forward_event_subagent_tool_calls_detected,);
case!(forward_event_subagent_tool_result,);
case!(forward_event_elicitation_requested,);
case!(forward_event_elicitation_resolved,);
case!(forward_event_turn_continued,);
case!(forward_event_retry_progress,);
case!(forward_event_session_aborted,);
case!(forward_artifacts,);
case!(forward_failed_twice,);
case!(forward_status_explicit,);
case!(forward_status_normal,);
case!(forward_status_turn_failed,);
case!(forward_status_hidden,);
case!(forward_status_severity,);
case!(forward_status_nullish,);
case!(forward_status_text,);
case!(forward_status_empty_kind,);
case!(forward_status_text_empty,);
case!(forward_status_text_false,);
case!(forward_status_text_zero,);
case!(forward_status_kind_false,);
case!(forward_status_kind_zero,);
case!(forward_status_detail_null,);
case!(forward_status_numeric_message,);
case!(forward_status_implicit_max_budget_reached,);
case!(forward_status_implicit_task_budget_reached,);
case!(forward_status_implicit_model_request_failed,);
case!(forward_status_implicit_tool_call_recovery_exhausted,);
case!(forward_status_implicit_max_turns_reached,);
case!(forward_status_implicit_max_output_recovery_exhausted,);
case!(forward_status_implicit_empty_response,);
case!(forward_status_implicit_content_filter,);
case!(forward_status_implicit_unknown_finish_reason,);
case!(forward_host_status,);
case!(forward_status_after_failed,);
case!(forward_status_write_error,);
case!(forward_catch_complete_once,);
case!(forward_catch_invalid_idle,);
case!(forward_catch_invalid_step,);

#[tokio::test]
async fn forward_completed() {
    let row = rows()
        .into_iter()
        .find(|r| r.get("name") == Some(&"completed".into()))
        .unwrap();
    let events = values(row.get("events").unwrap());
    assert_eq!(
        events[events.len() - 2].get("type"),
        Some(&"warning".into()),
        "TS delays loop completed"
    );
    let (_, handler, mut rx) = buffered_handler(4);
    deliver(&handler, &host_event("turn_started"), false)
        .await
        .unwrap();
    rx.try_recv().unwrap();
    let error = handler
        .validate_event(values(row.get("inputs").unwrap())[0].get("event").unwrap())
        .err()
        .unwrap();
    assert!(
        error.message.contains("must not reach"),
        "pr5d strict terminal guard retained"
    );
}

async fn status_pending(drop_owner: bool) {
    let name = if drop_owner {
        "status_drop"
    } else {
        "status_cancel"
    };
    let oracle = rows()
        .into_iter()
        .find(|r| r.get("name") == Some(&name.into()))
        .unwrap();
    assert_eq!(
        oracle.get("pending").unwrap().get("first"),
        Some(&JsValue::Bool(false))
    );
    let mut row = rows()
        .into_iter()
        .find(|r| r.get("name") == Some(&"status_explicit".into()))
        .unwrap()
        .as_object()
        .unwrap()
        .clone();
    row.insert("fault".into(), "agent_status_message".into());
    row.insert("noFail".into(), JsValue::Bool(true));
    let mut f = Fixture::new(&JsValue::Object(row));
    let (entered, mut entry) = oneshot::channel();
    let (release, wait) = std::sync::mpsc::channel();
    *f.store.gate.lock().unwrap() = Some((entered, wait));
    for _ in 0..2 {
        next(&mut f.turn).await.unwrap();
    }
    let mut pending = f.turn.next();
    timeout(Duration::from_secs(5), async {
        tokio::select! {result=&mut pending=>panic!("status escaped gate: {result:?}"), entered=&mut entry=>entered.unwrap()}
    }).await.expect("status writer must enter gate");
    drop(pending);
    for _ in 0..2 {
        let mut pending = f.turn.next();
        assert_recording_pending(pending.as_mut(), "status predecessor still pending");
    }
    assert_eq!(
        f.store.attempts.load(Ordering::SeqCst),
        1,
        "status write entered once"
    );
    assert!(
        f.session.lock().unwrap().active.is_some(),
        "status pending holds actual slot"
    );
    let runtime = f.registry.resolve(None).unwrap();
    assert_eq!(runtime.lease_count(), 1, "status pending holds lease");
    if drop_owner {
        let turn = std::mem::replace(
            &mut f.turn,
            stream(
                Box::pin(std::future::pending()),
                mpsc::channel(1).1,
                Arc::new(StreamIntegrity::new()),
            ),
        );
        drop(turn);
        assert!(
            f.session.lock().unwrap().active.is_some(),
            "status dropped worker holds slot"
        );
        assert_eq!(
            runtime.lease_count(),
            1,
            "status dropped worker holds lease"
        );
    }
    release.send(()).unwrap();
    if !drop_owner {
        while next(&mut f.turn)
            .await
            .expect("status resume retains pending transition")
            .is_some()
        {}
    }
    timeout(Duration::from_secs(5), async {
        while f.session.lock().unwrap().active.is_some() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(
        runtime.lease_count(),
        0,
        "status completed worker releases lease"
    );
    assert_eq!(
        f.store.attempts.load(Ordering::SeqCst),
        1,
        "status no replay after cancelled waiter"
    );
}
#[tokio::test]
async fn forward_status_cancel() {
    status_pending(false).await;
}
#[tokio::test]
async fn forward_status_drop() {
    status_pending(true).await;
}

case!(forward_malformed_null_first,);
case!(forward_malformed_null_visible,);
case!(forward_malformed_missing_first,);
case!(forward_malformed_missing_visible,);

case!(forward_malformed_host,);
case!(forward_malformed_pump,);

// The existing peer supplies status at sequence 3 and a queued successor/final.
// Run the real HostClient so a rejected receipt also proves its applied cursor.
async fn status_receipt(success: bool) {
    let name = if success {
        "status_receipt_success"
    } else {
        "status_receipt_failure"
    };
    let row = rows()
        .into_iter()
        .find(|r| r.get("name") == Some(&name.into()))
        .unwrap();
    assert_eq!(row.get("pendingAcks"), Some(&JsValue::Number(0.0)));
    assert_eq!(values(row.get("acks").unwrap()).len(), usize::from(success));
    let (inner, mut handler, mut rx) = buffered_handler(16);
    Arc::get_mut(&mut handler).unwrap().callback_cancel = Some(Cancellation::default());
    let client = spawn_client("live-normal").await.unwrap();
    let final_received = client.final_received();
    let child = client.child_id().unwrap();
    let (request, _) = parse_execute_request(&input(), &options(), vec![]).unwrap();
    let mut run = Box::pin(client.execute(request, handler));
    let mut receipt = None;
    timeout(FINAL_SYNC_TIMEOUT, async {
        loop {
            tokio::select! { biased;
                result = &mut run => panic!("status awaits durable receipt: {result:?}"),
                Some((event, _, done)) = rx.recv() => {
                    if event.get("type") == Some(&"agent_status".into()) { receipt = Some(done); }
                },
                _ = tokio::task::yield_now() => {},
            }
            if receipt.is_some() && final_received.load(Ordering::SeqCst) {
                break;
            }
        }
    })
    .await
    .expect("real host must reach status and receive final");
    assert_recording_pending(run.as_mut(), "status awaits durable receipt");
    assert_eq!(
        inner.events.lock().unwrap().len(),
        4,
        "status callback cannot release successor"
    );
    let receipt = receipt
        .unwrap()
        .expect("#741 pr5c status awaits durable receipt: status owns durable receipt");
    if success {
        receipt.send(()).unwrap();
    } else {
        drop(receipt);
    }
    let result = timeout(Duration::from_secs(5), run)
        .await
        .expect("receipt resolves real host");
    if success {
        assert!(
            matches!(result.unwrap(), ExecuteResult::Terminal(_)),
            "status success releases final"
        );
    } else {
        let error = result.unwrap_err();
        assert_eq!(error.kind, HostErrorKind::Callback);
        assert_eq!(
            error.last_applied_sequence,
            Some(2.0),
            "failed status leaves application cursor unchanged"
        );
    }
    assert_eq!(
        inner.disposed.load(Ordering::SeqCst),
        1,
        "status exit joins disposal"
    );
    assert_reaped(child).await;
}
#[tokio::test]
async fn forward_status_receipt_success() {
    status_receipt(true).await;
}
#[tokio::test]
async fn forward_status_receipt_failure() {
    status_receipt(false).await;
}

#[tokio::test(start_paused = true)]
async fn forward_pump_error_cleanup() {
    let row = rows()
        .into_iter()
        .find(|r| r.get("name") == Some(&"pump_error_cleanup".into()))
        .unwrap();
    assert_eq!(row.get("pendingCompletes"), Some(&JsValue::Number(0.0)));
    assert_eq!(row.get("closed"), Some(&JsValue::Bool(true)));
    let (inner, mut handler, _rx) = buffered_handler(4);
    let cancel = Cancellation::default();
    Arc::get_mut(&mut handler).unwrap().callback_cancel = Some(cancel.clone());
    inner
        .drains
        .lock()
        .unwrap()
        .push_back(Err(HostError::new(HostErrorKind::Callback, "pump failed")));
    let (entered, mut entry) = oneshot::channel();
    let (release, wait) = oneshot::channel();
    let owner = inner.clone();
    let (request, _) = parse_execute_request(&input(), &options(), vec![]).unwrap();
    let mut run = Box::pin(handler.with_pump(
        async move {
            cancel.cancelled().await;
            entered.send(()).unwrap();
            wait.await.unwrap();
            owner.dispose().await?;
            Ok(ExecuteResult::Aborted { terminal: None })
        },
        request.identity,
    ));
    timeout(Duration::from_secs(2), async {
        tokio::select! {result=&mut run=>panic!("pump error must await execution cleanup: {result:?}"), result=&mut entry=>result.unwrap()}
    }).await.expect("pump must cancel execution");
    assert_recording_pending(run.as_mut(), "pump error must await execution cleanup");
    assert_eq!(inner.disposed.load(Ordering::SeqCst), 0);
    release.send(()).unwrap();
    let error = timeout(Duration::from_secs(2), run)
        .await
        .unwrap()
        .unwrap_err();
    assert_eq!(error.message, "pump failed");
    assert_eq!(inner.disposed.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn forward_real_status_write_error() {
    let root = std::env::temp_dir().join(format!("pdrs-status-host-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&root).unwrap();
    let key = TranscriptKey {
        project_root: root.clone(),
        pilot_home: root.join("home"),
        session_id: "session".into(),
        transcript_path: root.join("session.jsonl"),
    };
    let store = Arc::new(RecordingStore {
        disk: JsonlTranscriptStore::new(),
        kind: "agent_status_message",
        fail: 1,
        completes: AtomicUsize::new(0),
        attempts: AtomicUsize::new(0),
        gate: Mutex::new(None),
    });
    let inner = Arc::new(Handler::default());
    let (mut session, children) = session_tracking_children("live-normal", inner.clone()).await;
    session.recorder = Arc::new(Recorder::new(
        Arc::new(OrderedStore(store.clone(), inner.clone())),
        key,
    ));
    let mut turn = session.submit(input(), options()).unwrap();
    let mut events = vec![];
    while let Some(event) = next(turn.as_mut()).await.unwrap() {
        events.push(event);
    }
    assert_eq!(
        events
            .iter()
            .map(|e| e.get("type").unwrap().clone())
            .collect::<Vec<_>>(),
        [
            "turn_started",
            "input_accepted",
            "model_event",
            "agent_status",
            "turn_failed",
            "turn_completed"
        ]
        .map(JsValue::from),
        "real status failure cuts off status and successors"
    );
    assert_eq!(
        events[4].get("error").unwrap().get("message"),
        Some(&"recording write failed".into()),
        "real status failure retains original error"
    );
    assert_eq!(
        store.attempts.load(Ordering::SeqCst),
        2,
        "real status catch persists unconditionally"
    );
    assert!(
        session.state.lock().unwrap().active.is_none(),
        "real status failure releases slot"
    );
    assert_eq!(children.lock().unwrap().len(), 1);
    let child = children.lock().unwrap()[0];
    assert_reaped(child).await;
    std::fs::remove_dir_all(root).unwrap();
}
