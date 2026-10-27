use super::*;
use crate::runtime::project::{ProjectRuntime, ProjectRuntimeRegistry, ResourceBundle};
use crate::storage::{
    JsonlTranscriptStore, StoreResult, TranscriptKey, TranscriptRead, TranscriptStore,
};

async fn next(turn: &mut dyn LiveTurn) -> Result<Option<JsValue>, RuntimeError> {
    timeout(Duration::from_secs(15), turn.next())
        .await
        .expect("#741h LiveTurn must make progress")
}

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
            .expect("#741h forward fixture invariant"),
    )
}
fn values(value: &JsValue) -> Vec<JsValue> {
    let JsValue::Array(values) = value else {
        panic!("#741h forward expected array")
    };
    values.clone()
}
// Only wall-clock fallback timestamps differ; writer IDs/timestamps are fixed.
fn clean(value: &mut JsValue) {
    let JsValue::Array(events) = value else {
        panic!("#741h forward expected event list")
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
                    panic!("#741h forward expected result object")
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
                "#741h loop cleanup before catch complete"
            );
        }
        self.0.append(key, entry)
    }
    fn read(&self, key: &TranscriptKey, limit: Option<u64>) -> StoreResult<TranscriptRead> {
        self.0.read(key, limit)
    }
}
// Observe independent slot/lease releases; neither EOF nor owner Drop is a
// barrier for the final lease. Match #842's 10-second bound and 5ms polling.
async fn wait_for_release(pending: impl Fn() -> bool) -> Result<(), tokio::time::error::Elapsed> {
    timeout(Duration::from_secs(10), async {
        while pending() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
}
pub(super) async fn wait_for_cleanup(session: &Mutex<SessionState>, runtime: &ProjectRuntime) {
    wait_for_release(|| {
        session
            .lock()
            .expect("#741h cleanup session lock")
            .active
            .is_some()
            || runtime.lease_count() != 0
    })
    .await
    .expect("#741h cleanup releases slot and lease within 10 seconds");
}

type CleanupGate = (oneshot::Sender<()>, std::sync::mpsc::Receiver<()>);
struct CleanupOwner {
    active: Option<Arc<ActiveTurn>>,
    gate: Arc<Mutex<Option<CleanupGate>>>,
}
impl Drop for CleanupOwner {
    fn drop(&mut self) {
        // Called by the real blocking worker's drop(owner), before drop(lease).
        // Arm only after the pending-owner assertions so premature release is
        // still killed by the original assertion rather than a gate timeout.
        self.active.take();
        if let Some((entered, wait)) = self.gate.lock().expect("#741h cleanup gate lock").take() {
            let _ = entered.send(());
            wait.recv_timeout(Duration::from_secs(10))
                .expect("#741h cleanup test must release owner gate");
        }
    }
}
struct ReleaseCleanup(Option<std::sync::mpsc::Sender<()>>);
impl ReleaseCleanup {
    fn release(&mut self) {
        if let Some(release) = self.0.take() {
            let _ = release.send(());
        }
    }
}
impl Drop for ReleaseCleanup {
    fn drop(&mut self) {
        self.release();
    }
}

struct Fixture {
    root: std::path::PathBuf,
    key: TranscriptKey,
    store: Arc<RecordingStore>,
    registry: Arc<ProjectRuntimeRegistry>,
    session: Arc<Mutex<SessionState>>,
    turn: LiveTurnStream,
    cleanup_gate: Arc<Mutex<Option<CleanupGate>>>,
}
impl Fixture {
    fn new(row: &JsValue) -> Self {
        let root = std::env::temp_dir().join(format!("pdrs-forward-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&root).expect("#741h forward fixture invariant");
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
        Arc::get_mut(&mut handler)
            .expect("#741h forward fixture invariant")
            .callback_cancel = Some(Cancellation::default());
        let checked = Arc::new(OrderedStore(store.clone(), inner.clone()));
        let writer = Recorder::new(checked, key.clone()).writer().clone();
        let ids = AtomicUsize::new(0);
        *writer
            .test_sources
            .lock()
            .expect("#741h forward fixture invariant") = Some((
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
        let inputs = values(row.get("inputs").expect("#741h forward fixture invariant"));
        let result = row
            .get("result")
            .expect("#741h forward fixture invariant")
            .clone();
        let open_step = row.get("name") == Some(&"catch_invalid_step".into());
        let pump = row.get("source") == Some(&"pump".into());
        let host_wait = row.get("source") == Some(&"host".into());
        let record = recorder.clone();
        let (ready, execute) = oneshot::channel();
        let mut turn = stream(
            Box::pin(async move {
                execute.await.expect("#741h forward fixture invariant");
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
                            .expect("#741h forward fixture invariant");
                    }
                    for item in inputs {
                        let event = item.get("event").expect("#741h forward fixture invariant");
                        if item.get("wire") == Some(&JsValue::Bool(true)) {
                            deliver(&handler, event, false).await?;
                        } else {
                            queue_host_events(&inner, std::slice::from_ref(event));
                            if pump {
                                let cancel = handler
                                    .callback_cancel
                                    .clone()
                                    .expect("#741h forward fixture invariant");
                                let owner = handler.clone();
                                return handler
                                    .with_pump(
                                        async move {
                                            cancel.cancelled().await;
                                            owner.dispose().await?;
                                            Ok(ExecuteResult::Aborted { terminal: None })
                                        },
                                        parse_execute_request(&input(), &options(), vec![])
                                            .expect("#741h forward fixture invariant")
                                            .0
                                            .identity,
                                    )
                                    .await;
                            }
                            if handler.module_call(JsValue::Null).await.is_err() || host_wait {
                                // Real HostClient sends a module error response, then observes cancel.
                                handler
                                    .callback_cancel
                                    .as_ref()
                                    .expect("#741h forward fixture invariant")
                                    .cancelled()
                                    .await;
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
            &parse_json(r#"{"type":"text","text":"probe"}"#)
                .expect("#741h forward fixture invariant"),
            &JsValue::Null,
            vec![],
            vec![],
        )
        .expect("#741h forward fixture invariant");
        let cleanup_gate = Arc::new(Mutex::new(None));
        turn.recording = Some(Recording::new(
            TurnRunner::new(
                recorder,
                Arc::new(
                    registry
                        .acquire(None)
                        .expect("#741h forward fixture invariant"),
                ),
                "s".into(),
                "t".into(),
            ),
            Arc::new(CleanupOwner {
                active: Some(active.clone()),
                gate: cleanup_gate.clone(),
            }),
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
            cleanup_gate,
        }
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.root).expect("#741h forward fixture invariant");
    }
}
async fn matrix(name: &str) {
    let row = rows()
        .into_iter()
        .find(|r| r.get("name") == Some(&name.into()))
        .expect("#741h forward fixture invariant");
    let mut f = Fixture::new(&row);
    let mut actual = Vec::new();
    while let Some(event) = next(&mut f.turn)
        .await
        .expect("#741h forward fixture invariant")
    {
        actual.push(event);
    }
    let mut expected = values(row.get("events").expect("#741h forward fixture invariant"));
    assert_eq!(
        expected.remove(2).get("type"),
        Some(&"user_prompt_submitted".into()),
        "#741h forward assertion"
    );
    let mut actual = JsValue::Array(actual);
    let mut expected = JsValue::Array(expected);
    clean(&mut actual);
    clean(&mut expected);
    assert_eq!(actual, expected, "#741h forward event order/payload");
    let bytes =
        std::fs::read_to_string(&f.key.transcript_path).expect("#741h forward fixture invariant");
    let expected = row
        .get("transcript")
        .expect("#741h forward fixture invariant")
        .as_string()
        .expect("#741h forward fixture invariant")
        .to_utf8()
        .expect("#741h forward fixture invariant");
    let parse = |s: &str| {
        let mut value = JsValue::Array(
            s.lines()
                .map(|line| parse_json(line).expect("#741h forward fixture invariant"))
                .collect(),
        );
        clean(&mut value);
        value
    };
    assert_eq!(
        parse(&bytes),
        parse(&expected),
        "#741h forward durable callback/complete"
    );
    let runtime = f
        .registry
        .resolve(None)
        .expect("#741h forward runtime exists");
    wait_for_cleanup(&f.session, &runtime).await;
    assert!(
        f.session
            .lock()
            .expect("#741h forward fixture invariant")
            .active
            .is_none(),
        "#741h forward actual slot released"
    );
    assert_eq!(runtime.lease_count(), 0, "#741h forward lease released");
}
macro_rules! case {
    ($test:ident,) => {
        #[tokio::test]
        async fn $test() {
            matrix(
                stringify!($test)
                    .strip_prefix("forward_")
                    .expect("#741h forward fixture invariant"),
            )
            .await;
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
        .expect("#741h forward fixture invariant");
    let events = values(row.get("events").expect("#741h forward fixture invariant"));
    assert_eq!(
        events[events.len() - 2].get("type"),
        Some(&"warning".into()),
        "#741h TS delays loop completed"
    );
    let (_, handler, mut rx) = buffered_handler(4);
    deliver(&handler, &host_event("turn_started"), false)
        .await
        .expect("#741h forward fixture invariant");
    rx.try_recv().expect("#741h forward fixture invariant");
    let error = handler
        .validate_event(
            values(row.get("inputs").expect("#741h forward fixture invariant"))[0]
                .get("event")
                .expect("#741h forward fixture invariant"),
        )
        .expect_err("#741h forward expected strict terminal rejection");
    assert!(
        error.message.contains("must not reach"),
        "#741h pr5d strict terminal guard retained"
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
        .expect("#741h forward fixture invariant");
    assert_eq!(
        oracle
            .get("pending")
            .expect("#741h forward fixture invariant")
            .get("first"),
        Some(&JsValue::Bool(false)),
        "#741h forward assertion"
    );
    let mut row = rows()
        .into_iter()
        .find(|r| r.get("name") == Some(&"status_explicit".into()))
        .expect("#741h forward fixture invariant")
        .as_object()
        .expect("#741h forward fixture invariant")
        .clone();
    row.insert("fault".into(), "agent_status_message".into());
    row.insert("noFail".into(), JsValue::Bool(true));
    let mut f = Fixture::new(&JsValue::Object(row));
    let (entered, mut entry) = oneshot::channel();
    let (release, wait) = std::sync::mpsc::channel();
    *f.store
        .gate
        .lock()
        .expect("#741h forward fixture invariant") = Some((entered, wait));
    for _ in 0..2 {
        next(&mut f.turn)
            .await
            .expect("#741h forward fixture invariant");
    }
    let mut pending = f.turn.next();
    timeout(Duration::from_secs(5), async {
        tokio::select! {result=&mut pending=>panic!("#741h status escaped gate: {result:?}"), entered=&mut entry=>entered.expect("#741h forward fixture invariant")}
    }).await.expect("#741h status writer must enter gate");
    drop(pending);
    for _ in 0..2 {
        let mut pending = f.turn.next();
        assert_recording_pending(pending.as_mut(), "#741h status predecessor still pending");
    }
    assert_eq!(
        f.store.attempts.load(Ordering::SeqCst),
        1,
        "#741h status write entered once"
    );
    assert!(
        f.session
            .lock()
            .expect("#741h forward fixture invariant")
            .active
            .is_some(),
        "#741h status pending holds actual slot"
    );
    let runtime = f
        .registry
        .resolve(None)
        .expect("#741h forward fixture invariant");
    assert_eq!(runtime.lease_count(), 1, "#741h status pending holds lease");
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
            f.session
                .lock()
                .expect("#741h forward fixture invariant")
                .active
                .is_some(),
            "#741h status dropped worker holds slot"
        );
        assert_eq!(
            runtime.lease_count(),
            1,
            "#741h status dropped worker holds lease"
        );
    }
    let (cleanup_entered, cleanup_entry) = oneshot::channel();
    let (cleanup_release, cleanup_wait) = std::sync::mpsc::channel();
    let mut cleanup_release = ReleaseCleanup(Some(cleanup_release));
    if drop_owner {
        *f.cleanup_gate.lock().expect("#741h cleanup gate lock") =
            Some((cleanup_entered, cleanup_wait));
    }
    release
        .send(())
        .expect("#741h status writer gate still open");
    if drop_owner {
        timeout(Duration::from_secs(10), cleanup_entry)
            .await
            .expect("#741h worker must reach owner release")
            .expect("#741h worker signals owner release");
        assert!(
            f.session
                .lock()
                .expect("#741h cleanup session lock")
                .active
                .is_none(),
            "#741h gated owner already released slot"
        );
        assert_eq!(
            runtime.lease_count(),
            1,
            "#741h gated worker still owns lease"
        );
        // The exact cleanup observer used below must wait even though the
        // active slot is already gone. Cancellation cannot release its lease.
        let mut waiting = Box::pin(wait_for_cleanup(&f.session, &runtime));
        assert_recording_pending(waiting.as_mut(), "#741h cleanup observer waits for lease");
        drop(waiting);
        assert_eq!(
            runtime.lease_count(),
            1,
            "#741h cancelled observer retains worker lease"
        );
        // No real time passes while proving an uncompleted predecessor has a
        // deadline. The blocking worker remains held by its explicit gate.
        tokio::time::pause();
        let mut waiting = Box::pin(wait_for_release(|| runtime.lease_count() != 0));
        assert_recording_pending(waiting.as_mut(), "#741h held lease observer stays pending");
        tokio::time::advance(Duration::from_secs(10)).await;
        // A live spawn_blocking worker prevents Tokio's idle auto-advance.
        // Drive the timer-wheel tick explicitly, then poll once (never await
        // another auto-advanced deadline while the worker gate is held).
        tokio::time::advance(Duration::from_millis(10)).await;
        std::future::poll_fn(|cx| {
            assert!(
                matches!(waiting.as_mut().poll(cx), std::task::Poll::Ready(Err(_))),
                "#741h cleanup observer reaches 10-second deadline"
            );
            std::task::Poll::Ready(())
        })
        .await;
        tokio::time::resume();
        assert_eq!(
            runtime.lease_count(),
            1,
            "#741h timed-out observer retains worker lease"
        );
        println!(
            "#741h slot=0 lease=1: gated worker; observer pending, cancelled, and bounded at 10s"
        );
        cleanup_release.release();
    }
    if !drop_owner {
        while next(&mut f.turn)
            .await
            .expect("#741h status resume retains pending transition")
            .is_some()
        {}
    }
    wait_for_cleanup(&f.session, &runtime).await;
    assert_eq!(
        runtime.lease_count(),
        0,
        "#741h status completed worker releases lease"
    );
    assert_eq!(
        f.store.attempts.load(Ordering::SeqCst),
        1,
        "#741h status no replay after cancelled waiter"
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
        .expect("#741h forward fixture invariant");
    assert_eq!(
        row.get("pendingAcks"),
        Some(&JsValue::Number(0.0)),
        "#741h forward assertion"
    );
    assert_eq!(
        values(row.get("acks").expect("#741h forward fixture invariant")).len(),
        usize::from(success),
        "#741h forward assertion"
    );
    let (inner, mut handler, mut rx) = buffered_handler(16);
    Arc::get_mut(&mut handler)
        .expect("#741h forward fixture invariant")
        .callback_cancel = Some(Cancellation::default());
    let client = spawn_client("live-normal")
        .await
        .expect("#741h forward fixture invariant");
    let final_received = client.final_received();
    let child = client.child_id().expect("#741h forward fixture invariant");
    let (request, _) = parse_execute_request(&input(), &options(), vec![])
        .expect("#741h forward fixture invariant");
    let mut run = Box::pin(client.execute(request, handler));
    let mut receipt = None;
    timeout(FINAL_SYNC_TIMEOUT, async {
        loop {
            tokio::select! { biased;
                result = &mut run => panic!("#741h status awaits durable receipt: {result:?}"),
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
    .expect("#741h real host must reach status and receive final");
    assert_recording_pending(run.as_mut(), "#741h status awaits durable receipt");
    assert_eq!(
        inner
            .events
            .lock()
            .expect("#741h forward fixture invariant")
            .len(),
        4,
        "#741h status callback cannot release successor"
    );
    let receipt = receipt
        .expect("#741h forward fixture invariant")
        .expect("#741h status awaits durable receipt: status owns durable receipt");
    if success {
        receipt.send(()).expect("#741h forward fixture invariant");
    } else {
        drop(receipt);
    }
    let result = timeout(Duration::from_secs(5), run)
        .await
        .expect("#741h receipt resolves real host");
    if success {
        assert!(
            matches!(
                result.expect("#741h forward fixture invariant"),
                ExecuteResult::Terminal(_)
            ),
            "#741h status success releases final"
        );
    } else {
        let error = result.expect_err("#741h forward expected failure");
        assert_eq!(
            error.kind,
            HostErrorKind::Callback,
            "#741h forward assertion"
        );
        assert_eq!(
            error.last_applied_sequence,
            Some(2.0),
            "#741h failed status leaves application cursor unchanged"
        );
    }
    assert_eq!(
        inner.disposed.load(Ordering::SeqCst),
        1,
        "#741h status exit joins disposal"
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
        .expect("#741h forward fixture invariant");
    assert_eq!(
        row.get("pendingCompletes"),
        Some(&JsValue::Number(0.0)),
        "#741h forward assertion"
    );
    assert_eq!(
        row.get("closed"),
        Some(&JsValue::Bool(true)),
        "#741h forward assertion"
    );
    let (inner, mut handler, _rx) = buffered_handler(4);
    let cancel = Cancellation::default();
    Arc::get_mut(&mut handler)
        .expect("#741h forward fixture invariant")
        .callback_cancel = Some(cancel.clone());
    inner
        .drains
        .lock()
        .expect("#741h forward fixture invariant")
        .push_back(Err(HostError::new(HostErrorKind::Callback, "pump failed")));
    let (entered, mut entry) = oneshot::channel();
    let (release, wait) = oneshot::channel();
    let owner = inner.clone();
    let (request, _) = parse_execute_request(&input(), &options(), vec![])
        .expect("#741h forward fixture invariant");
    let mut run = Box::pin(handler.with_pump(
        async move {
            cancel.cancelled().await;
            entered.send(()).expect("#741h forward fixture invariant");
            wait.await.expect("#741h forward fixture invariant");
            owner.dispose().await?;
            Ok(ExecuteResult::Aborted { terminal: None })
        },
        request.identity,
    ));
    timeout(Duration::from_secs(2), async {
        tokio::select! {result=&mut run=>panic!("#741h pump error must await execution cleanup: {result:?}"), result=&mut entry=>result.expect("#741h forward fixture invariant")}
    }).await.expect("#741h pump must cancel execution");
    assert_recording_pending(
        run.as_mut(),
        "#741h pump error must await execution cleanup",
    );
    assert_eq!(
        inner.disposed.load(Ordering::SeqCst),
        0,
        "#741h forward assertion"
    );
    release.send(()).expect("#741h forward fixture invariant");
    let error = timeout(Duration::from_secs(2), run)
        .await
        .expect("#741h forward fixture invariant")
        .expect_err("#741h forward expected failure");
    assert_eq!(error.message, "pump failed", "#741h forward assertion");
    assert_eq!(
        inner.disposed.load(Ordering::SeqCst),
        1,
        "#741h forward assertion"
    );
}

#[tokio::test]
async fn forward_real_status_write_error() {
    let root = std::env::temp_dir().join(format!("pdrs-status-host-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&root).expect("#741h forward fixture invariant");
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
    let mut turn = session
        .submit(input(), options())
        .expect("#741h forward fixture invariant");
    let mut events = vec![];
    while let Some(event) = next(turn.as_mut())
        .await
        .expect("#741h forward fixture invariant")
    {
        events.push(event);
    }
    assert_eq!(
        events
            .iter()
            .map(|e| e
                .get("type")
                .expect("#741h forward fixture invariant")
                .clone())
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
        "#741h real status failure cuts off status and successors"
    );
    assert_eq!(
        events[4]
            .get("error")
            .expect("#741h forward fixture invariant")
            .get("message"),
        Some(&"recording write failed".into()),
        "#741h real status failure retains original error"
    );
    assert_eq!(
        store.attempts.load(Ordering::SeqCst),
        2,
        "#741h real status catch persists unconditionally"
    );
    wait_for_release(|| {
        session
            .state
            .lock()
            .expect("#741h real status session lock")
            .active
            .is_some()
    })
    .await
    .expect("#741h real status failure releases slot within 10 seconds");
    assert!(
        session
            .state
            .lock()
            .expect("#741h forward fixture invariant")
            .active
            .is_none(),
        "#741h real status failure releases slot"
    );
    assert_eq!(
        children
            .lock()
            .expect("#741h forward fixture invariant")
            .len(),
        1,
        "#741h forward assertion"
    );
    let child = children.lock().expect("#741h forward fixture invariant")[0];
    assert_reaped(child).await;
    std::fs::remove_dir_all(root).expect("#741h forward fixture invariant");
}
