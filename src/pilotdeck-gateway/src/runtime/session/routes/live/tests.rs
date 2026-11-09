use super::*;
use crate::runtime::session::routes::live::listing_prompt;
use crate::{
    host::{HostHandler, modules::event::EventSink},
    storage::{AsyncTranscriptAdapter, JsonlTranscriptStore, SessionStorageInput},
};
use pilotdeck_protocol::parse_json;
use std::{fs, path::PathBuf};

struct Fixture {
    root: PathBuf,
    path: PathBuf,
    state: Arc<LiveSessionState>,
}

impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!("pdrs-live-bridge-{}", uuid::Uuid::new_v4()));
        let home = root.join("home");
        let adapter = AsyncTranscriptAdapter::new(
            Arc::new(JsonlTranscriptStore::new()),
            Arc::new(|key| key.pilot_home.join("chats")),
        );
        let storage = adapter.create_session_storage(SessionStorageInput {
            project_root: root.clone(),
            pilot_home: home,
            session_id: "s".into(),
            now: None,
        });
        let path = storage.transcript_path.clone();
        let state =
            LiveSessionState::new("s".into(), root.to_string_lossy().as_ref().into(), storage)
                .unwrap();
        Self { root, path, state }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn input() -> JsValue {
    parse_json(
        r#"{"sessionId":"s","turnId":"t","agent":{"cwd":"/tmp","permissionMode":"default"}}"#,
    )
    .unwrap()
}

#[test]
fn context_budget_event_persists_the_real_projected_snapshot() {
    let fixture = Fixture::new();
    let manager = crate::runtime::model::token_accounting::TokenBudgetManager::default();
    let request = parse_json(r#"{"messages":[{"role":"user","content":[{"type":"text","text":"Actual budget message"}]}]}"#).unwrap();
    let breakdown = manager
        .estimate_request_breakdown(request.as_object().unwrap())
        .unwrap();
    let tokens = breakdown.get("total").and_then(JsValue::as_number).unwrap();
    let mut metadata = JsObject::new();
    metadata.insert("reservedOutputTokens".into(), JsValue::Number(32768.0));
    let snapshot = manager.snapshot_from_tokens(tokens, 128000.0, &metadata);
    fixture
        .state
        .record_event(&crate::metadata::object([
            ("type", "context_budget".into()),
            ("sessionId", "s".into()),
            ("turnId", "t".into()),
            ("snapshot", snapshot),
        ]))
        .unwrap();
    let entries = crate::storage::read_transcript(&fixture.path)
        .unwrap()
        .entries;
    assert_eq!(entries.len(), 1);
    let detail = entries[0].get("detail").unwrap();
    assert_eq!(detail.get("used"), Some(&JsValue::Number(tokens)));
    assert_eq!(
        detail.get("reservedOutputTokens"),
        Some(&JsValue::Number(32768.0))
    );
    assert_eq!(
        detail.get("effectiveTotal"),
        Some(&JsValue::Number(95232.0))
    );
}

#[test]
fn listing_metadata_records_human_prompt_and_reappends_snapshot_once() {
    let fixture = Fixture::new();
    fixture
        .state
        .record_event(
            &parse_json(r#"{"type":"turn_started","sessionId":"s","turnId":"t"}"#).unwrap(),
        )
        .unwrap();
    fixture.state.record_event(&parse_json(r#"{"type":"input_accepted","sessionId":"s","turnId":"t","messages":[{"role":"user","metadata":{"synthetic":true},"content":[{"type":"text","text":"hidden"}]},{"role":"user","content":[{"type":"text","text":"  actual prompt  "}]}]}"#).unwrap()).unwrap();
    let result = parse_json(
        r#"{"sessionId":"s","turnId":"t","type":"success","usage":{},"permissionDenials":[]}"#,
    )
    .unwrap();
    fixture.state.complete_turn(&result).unwrap();
    fixture.state.complete_turn(&result).unwrap();
    let entries = crate::storage::read_transcript(&fixture.path)
        .unwrap()
        .entries;
    let metadata: Vec<_> = entries
        .iter()
        .filter(|e| e.get("type") == Some(&"session_metadata".into()))
        .map(|e| e.get("metadata").unwrap())
        .collect();
    assert_eq!(metadata.len(), 2);
    for entry in &metadata {
        assert_eq!(entry.get("firstPrompt"), Some(&"actual prompt".into()));
        assert_eq!(entry.get("lastPrompt"), Some(&"actual prompt".into()));
        assert!(
            entry
                .get("updatedAt")
                .and_then(JsValue::as_string)
                .is_some_and(|s| !s.is_empty())
        );
    }
    assert_eq!(metadata[1].get("isSnapshot"), Some(&JsValue::Bool(true)));
    assert_eq!(
        listing_prompt(&[
            parse_json(r#"{"role":"user","content":[{"type":"text","text":"😀"}]}"#).unwrap()
        ])
        .units()
        .len(),
        2
    );
}

#[tokio::test]
async fn sidecar_steer_uses_the_live_recorders_existing_writer() {
    use crate::runtime::session::{
        LiveSession,
        sidecar_session::{SidecarSession, SidecarSessionConfig, SidecarSpawner},
    };
    struct Exit;
    impl SidecarSpawner for Exit {
        fn requires_subprocess(&self) -> bool {
            true
        }

        fn command(&self) -> std::io::Result<tokio::process::Command> {
            Ok(tokio::process::Command::new("false"))
        }
    }
    let fixture = Fixture::new();
    let writer = fixture.state.recorder.writer().clone();
    let mut mutation = JsObject::new();
    mutation.insert("type".into(), "inbox_mutation".into());
    mutation.insert("mutation".into(), "insert".into());
    mutation.insert("itemId".into(), "before-turn".into());
    mutation.insert("message".into(), JsValue::Object(JsObject::new()));
    assert!(
        writer
            .record_native_session_event(&"s".into(), &"t".into(), mutation)
            .is_err()
    );
    assert_eq!(writer.snapshot_state().sequence, 0);
    fixture
        .state
        .record_event(
            &parse_json(r#"{"type":"turn_started","sessionId":"s","turnId":"t"}"#).unwrap(),
        )
        .unwrap();
    let session = SidecarSession::new(
        SidecarSessionConfig {
            limits: crate::host::Limits::default(),
            session_key: "s".into(),
            project_key: None,
            cwd: fixture.root.to_string_lossy().as_ref().into(),
            snapshot: JsValue::Null,
            services: None,
        },
        Arc::new(Exit),
    );
    let mut turn = with_live_state(fixture.state.clone(), || session.submit(parse_json(r#"{"agent":{},"messages":[{"role":"user","content":[{"type":"text","text":"hi"}]}]}"#).unwrap(), parse_json(r#"{"turnId":"t","operationId":"op"}"#).unwrap())).unwrap();
    let result = session
        .steer(
            "t".into(),
            crate::runtime::steer::SteerMessage {
                item_id: "i".into(),
                message: parse_json(
                    r#"{"role":"user","content":[{"type":"text","text":"steer"}]}"#,
                )
                .unwrap(),
                allowed_read_files: None,
            },
        )
        .await
        .unwrap();
    assert!(result.accepted);
    assert!(
        session
            .cancel_steer("t".into(), "i".into())
            .await
            .unwrap()
            .cancelled
    );
    let entries = crate::storage::read_transcript(&fixture.path)
        .unwrap()
        .entries;
    assert!(
        entries
            .iter()
            .any(|e| e.get("type") == Some(&"inbox_mutation".into()))
    );
    assert_eq!(
        fixture.state.recorder.writer().snapshot_state().sequence as usize,
        entries.len()
    );
    assert!(Arc::ptr_eq(&writer, fixture.state.recorder.writer()));
    while let Ok(Some(_)) = turn.next().await {}
    session.dispose().await.unwrap();
}

#[test]
fn cold_seed_is_parsed_and_bad_seed_fails_closed() {
    let seeded = parse_json(r#"{"seedState":{"allowedReadFiles":["/tmp/seed"],"readFileState":{"/tmp/seed::text::1::all::":{"mtimeMs":1,"kind":"text"}},"writeSnapshots":{"/tmp/seed":{"absolutePath":"/tmp/seed","mtimeMs":1,"contentHash":"hash"}}}}"#).unwrap();
    let checkpoint = checkpoint(&seeded, None).unwrap();
    let snapshot = checkpoint.snapshot().unwrap();
    assert_eq!(
        snapshot.allowed_read_files.unwrap(),
        vec![JsString::from("/tmp/seed")]
    );
    assert!(
        snapshot
            .read_file_state
            .unwrap()
            .contains_key("/tmp/seed::text::1::all::")
    );
    assert!(snapshot.write_snapshots.unwrap().contains_key("/tmp/seed"));
    assert!(
        checkpoint_without_seed()
            .read_file_state
            .lock()
            .unwrap()
            .is_empty()
    );
    let malformed = parse_json(r#"{"seedState":{"allowedReadFiles":42}}"#).unwrap();
    assert!(super::checkpoint(&malformed, None).is_err());
}

fn checkpoint_without_seed() -> HostToolCheckpoint {
    checkpoint(&input(), None).unwrap()
}

#[test]
fn scoped_checkpoint_preserves_maps_and_steer_uses_the_same_checkpoint() {
    let fixture = Fixture::new();
    let captured = with_live_state(fixture.state.clone(), || {
        assert!(Arc::ptr_eq(&live_state().unwrap(), &fixture.state));
        let router = build(
            JsValue::Null,
            &input(),
            Arc::new(EventBuffer::default()),
            crate::runtime::network::Cancellation::default(),
        )
        .unwrap();
        router
            .turn
            .authorizations
            .as_ref()
            .unwrap()
            .remember("item".into(), vec!["/tmp/granted".into()]);
        router.turn.apply_steer_authorization(&"item".into());
        checkpoint(&input(), live_state().as_deref()).unwrap()
    });
    assert!(live_state().is_none());
    assert!(Arc::ptr_eq(
        &captured.read_file_state,
        &fixture.state.checkpoint.read_file_state
    ));
    assert!(Arc::ptr_eq(
        &captured.write_snapshots,
        &fixture.state.checkpoint.write_snapshots
    ));
    assert_eq!(
        fixture
            .state
            .checkpoint
            .snapshot()
            .unwrap()
            .allowed_read_files
            .unwrap(),
        vec![JsString::from("/tmp/granted")]
    );
}

#[test]
fn scope_restores_on_nested_call_error_and_unwind() {
    let outer = Fixture::new();
    let inner = Fixture::new();
    with_live_state(outer.state.clone(), || {
        let result: Result<(), &str> = with_live_state(inner.state.clone(), || {
            assert!(Arc::ptr_eq(&live_state().unwrap(), &inner.state));
            Err("submission failed")
        });
        assert!(result.is_err());
        assert!(Arc::ptr_eq(&live_state().unwrap(), &outer.state));
        let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            with_live_state(inner.state.clone(), || panic!("submission panic"));
        }));
        assert!(panic.is_err());
        assert!(Arc::ptr_eq(&live_state().unwrap(), &outer.state));
    });
    assert!(live_state().is_none());
}

#[test]
fn scope_does_not_leak_to_another_thread_or_accept_an_invalid_wire_seed() {
    let fixture = Fixture::new();
    with_live_state(fixture.state.clone(), || {
        std::thread::spawn(|| assert!(live_state().is_none()))
            .join()
            .unwrap();
        let bad = parse_json(r#"{"seedState":{"writeSnapshots":[]}}"#).unwrap();
        assert!(checkpoint(&bad, live_state().as_deref()).is_err());
    });
}

#[tokio::test]
async fn async_router_retains_ports_and_records_before_delivery_with_shared_sequence() {
    let fixture = Fixture::new();
    let buffer = Arc::new(EventBuffer::default());
    let router = with_live_state(fixture.state.clone(), || {
        build(
            JsValue::Null,
            &input(),
            buffer.clone(),
            crate::runtime::network::Cancellation::default(),
        )
        .unwrap()
    });
    assert!(live_state().is_none());
    let call = parse_json(r#"{"module":"event","payload":{"operation":"emit","event":{"type":"agent_status","sessionId":"s","turnId":"t","event":"running","text":"working"}}}"#).unwrap();
    router.module_call(call.clone()).await.unwrap();
    assert_eq!(buffer.drain().len(), 1);
    let status = parse_json(r#"{"event":"route_status","text":"second producer"}"#).unwrap();
    fixture
        .state
        .recorder
        .writer()
        .record_agent_status_message(&"s".into(), &"t".into(), &status)
        .unwrap();
    router.module_call(call.clone()).await.unwrap();
    let read = crate::storage::read_transcript(&fixture.path).unwrap();
    assert_eq!(read.entries.len(), 3);
    assert_eq!(fixture.state.recorder.snapshot_state().0, 3);
    for (index, entry) in read.entries.iter().enumerate() {
        assert_eq!(
            entry.get("sequence").and_then(JsValue::as_number),
            Some((index + 1) as f64)
        );
    }
    assert!(
        read.diagnostics
            .iter()
            .all(|diagnostic| diagnostic.severity != "error")
    );
    buffer.drain();
    fs::remove_file(&fixture.path).unwrap();
    fs::create_dir(&fixture.path).unwrap();
    assert!(router.module_call(call).await.is_err());
    assert!(buffer.drain().is_empty());
    assert_eq!(fixture.state.recorder.snapshot_state().0, 3);
    assert!(fixture.state.check_error().is_err());
}

#[test]
fn real_events_share_writer_messages_and_completed_history() {
    let fixture = Fixture::new();
    let emit = |json: &str| {
        let event = parse_json(json).unwrap();
        fixture.state.record_event(&event).unwrap();
    };
    emit(r#"{"type":"turn_started","sessionId":"s","turnId":"t"}"#);
    emit(
        r#"{"type":"input_accepted","sessionId":"s","turnId":"t","messages":[{"role":"user","content":[{"type":"text","text":"hi"}]}]}"#,
    );
    fixture
        .state
        .recorder
        .record_model_request(&"s".into(), &"t".into(), JsValue::Null)
        .unwrap();
    emit(
        r#"{"type":"assistant_message","sessionId":"s","turnId":"t","message":{"role":"assistant","content":[{"type":"text","text":"hello"}]}}"#,
    );
    emit(
        r#"{"type":"tool_results_projected","sessionId":"s","turnId":"t","message":{"role":"user","content":[{"type":"tool_result","toolCallId":"call","content":[]}]}}"#,
    );
    assert!(
        fixture
            .state
            .record_status(
                &"t".into(),
                &parse_json(r#"{"event":"working","text":"saved status"}"#).unwrap()
            )
            .unwrap()
    );
    emit(
        r#"{"type":"turn_completed","sessionId":"s","turnId":"t","result":{"sessionId":"s","turnId":"t","type":"success","usage":{},"permissionDenials":[]}}"#,
    );
    let snapshot = fixture.state.snapshot().unwrap();
    let Some(JsValue::Array(messages)) = snapshot.get("messages") else {
        panic!("messages required");
    };
    assert_eq!(messages.len(), 3);
    assert_eq!(
        crate::runtime::turn::history::completed_messages(&fixture.path).unwrap(),
        *messages
    );
    assert!(
        matches!(snapshot.get("statuses"), Some(JsValue::Array(statuses)) if statuses.len() == 1)
    );
    let entries = crate::storage::read_transcript(&fixture.path)
        .unwrap()
        .entries;
    for (index, entry) in entries.iter().enumerate() {
        assert_eq!(
            entry.get("sequence").and_then(JsValue::as_number),
            Some((index + 1) as f64)
        );
        if index > 0 {
            assert_eq!(
                entry.get("parentEntryId"),
                entries[index - 1].get("entryId")
            );
        }
    }
}

#[test]
fn disposed_status_refuses_without_mutating_disk_and_weak_bindings_expire() {
    let fixture = Fixture::new();
    let bindings = crate::runtime::session::routes::live::LiveBindings::default();
    bindings.bind(&fixture.state);
    assert!(Arc::ptr_eq(
        &bindings.current(&"s".into()).unwrap(),
        &fixture.state
    ));
    let status = parse_json(r#"{"event":"status","text":"once"}"#).unwrap();
    fixture.state.record_status(&"t".into(), &status).unwrap();
    let before = fs::read(&fixture.path).unwrap();
    fixture.state.dispose().unwrap();
    assert!(!fixture.state.record_status(&"t".into(), &status).unwrap());
    assert_eq!(fs::read(&fixture.path).unwrap(), before);
    assert!(bindings.current(&"s".into()).is_none());
}

#[tokio::test]
async fn publication_fence_blocks_cold_writer_until_factory_binding_is_visible() {
    let fixture = Fixture::new();
    let bindings = Arc::new(crate::runtime::session::routes::live::LiveBindings::default());
    let factory_guard = bindings.publication.lock().await;
    let waiting = bindings.clone();
    let cold = tokio::spawn(async move {
        let _guard = waiting.publication.lock().await;
        // A state without a published owner must be treated as publishing,
        // never as a cold session. The production host also checks identity.
        waiting.current(&"s".into()).is_some()
    });
    bindings.bind(&fixture.state);
    assert!(!cold.is_finished());
    drop(factory_guard);
    assert!(cold.await.unwrap());
}

#[test]
fn replacement_lookup_preserves_new_identity_when_old_state_disposes() {
    let old = Fixture::new();
    let replacement = Fixture::new();
    let bindings = crate::runtime::session::routes::live::LiveBindings::default();
    bindings.bind(&old.state);
    bindings.bind(&replacement.state);
    old.state.dispose().unwrap();
    assert!(Arc::ptr_eq(
        &bindings.current(&"s".into()).unwrap(),
        &replacement.state
    ));
    assert_ne!(old.state.identity, replacement.state.identity);
}

#[tokio::test]
async fn direct_host_events_record_and_latch_failures_in_the_same_buffer() {
    let fixture = Fixture::new();
    let buffer = Arc::new(EventBuffer::default());
    let _router = with_live_state(fixture.state.clone(), || {
        build(
            JsValue::Null,
            &input(),
            buffer.clone(),
            crate::runtime::network::Cancellation::default(),
        )
        .unwrap()
    });
    let event =
        parse_json(r#"{"type":"agent_status","sessionId":"s","turnId":"t","event":"running"}"#)
            .unwrap()
            .as_object()
            .unwrap()
            .clone();
    EventSink::emit(buffer.as_ref(), event.clone()).unwrap();
    assert_eq!(fixture.state.recorder.snapshot_state().0, 1);
    assert_eq!(buffer.drain().len(), 1);
    fs::remove_file(&fixture.path).unwrap();
    fs::create_dir(&fixture.path).unwrap();
    assert!(EventSink::emit(buffer.as_ref(), event).is_err());
    assert!(fixture.state.check_error().is_err());
    assert!(buffer.drain().is_empty());
}

struct BufferedStream {
    events: std::collections::VecDeque<JsValue>,
}
impl model::ModelStream for BufferedStream {
    fn next(&mut self) -> model::StreamFuture<'_, Option<JsValue>> {
        Box::pin(async move { Ok(self.events.pop_front()) })
    }
}

struct LocalModel;
impl model::ModelExecution for LocalModel {
    fn prepare(&self, input: model::PrepareInput) -> model::ModelFuture<model::PreparedInvocation> {
        Box::pin(async move {
            Ok(model::PreparedInvocation::new(
                input.request,
                "test".into(),
                "local".into(),
            ))
        })
    }
    fn stream(
        &self,
        _input: model::StreamInput,
    ) -> model::ModelFuture<Box<dyn model::ModelStream>> {
        Box::pin(async {
            Ok(Box::new(BufferedStream {
                events: vec![parse_json(r#"{"type":"text_delta","text":"hello"}"#).unwrap()].into(),
            }) as Box<dyn model::ModelStream>)
        })
    }
}

#[tokio::test]
async fn durable_model_port_delegates_and_records_before_returning_events() {
    let fixture = Fixture::new();
    fixture
        .state
        .recorder
        .start_turn(&"s".into(), &"t".into())
        .unwrap();
    let port = RecordedModel {
        inner: Arc::new(LocalModel),
        state: fixture.state.clone(),
        session: "s".into(),
        turn: "t".into(),
    };
    let prepared = model::ModelExecution::prepare(
        &port,
        model::PrepareInput {
            request: JsValue::Null,
            context: JsObject::new(),
        },
    )
    .await
    .unwrap();
    let mut stream = model::ModelExecution::stream(
        &port,
        model::StreamInput {
            prepared,
            context: JsObject::new(),
        },
    )
    .await
    .unwrap();
    assert!(stream.next().await.unwrap().is_some());
    assert!(stream.next().await.unwrap().is_none());
    let read = crate::storage::read_transcript(&fixture.path).unwrap();
    assert_eq!(read.entries.len(), 4);
    assert_eq!(
        read.entries[2].get("type"),
        Some(&JsValue::from("model_request"))
    );
    assert_eq!(
        read.entries[3].get("type"),
        Some(&JsValue::from("model_stream_event"))
    );
}

#[tokio::test]
async fn real_read_tool_updates_the_seeded_maps_and_uses_the_shared_recorder() {
    let fixture = Fixture::new();
    fs::create_dir_all(&fixture.root).unwrap();
    let path = fixture.root.join("read.txt");
    fs::write(&path, "actual file contents").unwrap();
    let absolute: JsString = path.to_string_lossy().as_ref().into();
    fixture
        .state
        .checkpoint
        .allow_read_files(std::slice::from_ref(&absolute));
    fixture
        .state
        .recorder
        .start_turn(&"s".into(), &"t".into())
        .unwrap();
    fixture
        .state
        .recorder
        .record_model_request(&"s".into(), &"t".into(), JsValue::Null)
        .unwrap();
    let mut config = input();
    if let JsValue::Object(fields) = &mut config {
        fields.insert("seedState".into(), fixture.state.seed_state().unwrap());
        let mut agent = fields
            .get("agent")
            .and_then(JsValue::as_object)
            .cloned()
            .unwrap();
        agent.insert("cwd".into(), fixture.root.to_string_lossy().as_ref().into());
        fields.insert("agent".into(), JsValue::Object(agent));
    }
    let router = with_live_state(fixture.state.clone(), || {
        build(
            JsValue::Null,
            &config,
            Arc::new(EventBuffer::default()),
            crate::runtime::network::Cancellation::default(),
        )
        .unwrap()
    });
    let mut payload = JsObject::new();
    payload.insert("operation".into(), "execute".into());
    payload.insert("toolCallId".into(), "read-call".into());
    payload.insert("name".into(), "read_file".into());
    let mut args = JsObject::new();
    args.insert("file_path".into(), JsValue::String(absolute.clone()));
    payload.insert("arguments".into(), JsValue::Object(args));
    let mut call = JsObject::new();
    call.insert("module".into(), "capability".into());
    call.insert("payload".into(), JsValue::Object(payload));
    let result = router.module_call(JsValue::Object(call)).await.unwrap();
    assert_eq!(result.get("toolCallId"), Some(&JsValue::from("read-call")));
    assert!(
        !fixture
            .state
            .checkpoint
            .read_file_state
            .lock()
            .unwrap()
            .is_empty()
    );
    assert!(
        fixture
            .state
            .checkpoint
            .write_snapshots
            .lock()
            .unwrap()
            .get_key(&absolute)
            .is_some()
    );
    let read = crate::storage::read_transcript(&fixture.path).unwrap();
    assert_eq!(
        read.entries[3].get("type"),
        Some(&JsValue::from("tool_call"))
    );
    assert_eq!(
        read.entries[4].get("type"),
        Some(&JsValue::from("tool_result"))
    );
    assert_eq!(
        read.entries[5].get("type"),
        Some(&JsValue::from("step_completed"))
    );
}

#[test]
fn existing_storage_rejects_turn_completion_after_real_turn_start() {
    let fixture = Fixture::new();
    fixture
        .state
        .recorder
        .start_turn(&"s".into(), &"t".into())
        .unwrap();
    let result = parse_json(
        r#"{"sessionId":"s","turnId":"t","type":"success","usage":{},"permissionDenials":[]}"#,
    )
    .unwrap();
    let error = fixture
        .state
        .recorder
        .writer()
        .record_turn_result(&"s".into(), &"t".into(), result)
        .unwrap_err();
    assert!(error.to_string().contains("turn_result"));
    assert_eq!(fixture.state.recorder.snapshot_state().0, 1);
    assert_eq!(
        crate::storage::read_transcript(&fixture.path)
            .unwrap()
            .entries
            .len(),
        1
    );
}
