//! Real registry/lease + turn binding + stdio LiveSession path; no sockets.
#[path = "interaction_fresh_router_tests.rs"]
mod fresh_router;
#[path = "interaction_prompt_tests.rs"]
mod prompt_hooks;
use super::*;
use crate::{
    host::{
        HostClient, HostError, HostFuture, HostHandler, Limits,
        modules::{ModuleRouter, capability, model},
    },
    runtime::{
        hooks::Cancellation,
        permission::{self, DefaultPermissionContextOptions},
        project::{RuntimeState, interaction_permission::PermissionTurnOptions},
        session::LiveSession,
        steer::MutationRecorder,
        turn::{
            interaction::{InteractionTurn, InteractionTurnInput},
            live::{InteractionHost, LiveSessionAdapter, test_recorder},
        },
    },
};
use pilotdeck_protocol::{admission::ExecutionIdentity, parse_json};
use tokio::process::Command;

struct Ports;
impl model::ModelExecution for Ports {
    fn prepare(&self, _: model::PrepareInput) -> model::ModelFuture<model::PreparedInvocation> {
        Box::pin(async { panic!("unused model") })
    }
    fn stream(&self, _: model::StreamInput) -> model::ModelFuture<Box<dyn model::ModelStream>> {
        Box::pin(async { panic!("unused model") })
    }
}
impl capability::ToolCatalog for Ports {
    fn list(&self) -> HostFuture<Vec<Arc<dyn capability::ToolDefinition>>> {
        Box::pin(async {
            Ok(vec![
                Arc::new(crate::runtime::tools::builtin::bash::BashTool::new())
                    as Arc<dyn capability::ToolDefinition>,
            ])
        })
    }
}
impl capability::ToolExecution for Ports {
    fn execute_all(
        &self,
        _: Vec<capability::ToolCall>,
        _: capability::ToolInvocationContext,
    ) -> HostFuture<Vec<JsObject>> {
        Box::pin(async { panic!("unused tools") })
    }
}
impl capability::ToolContext for Ports {
    fn prepare(&self, _: JsValue) -> HostFuture<capability::ToolInvocationContext> {
        Box::pin(async { panic!("unused context") })
    }
}
impl capability::CapabilityResultObserver for Ports {
    fn on_capability_results(&self, _: Vec<JsObject>) -> HostFuture<()> {
        Box::pin(async { Ok(()) })
    }
}
impl MutationRecorder for Ports {
    fn record(&self, _: JsString, _: JsObject) -> HostFuture<()> {
        Box::pin(async { Ok(()) })
    }
}
fn router(identity: &ExecutionIdentity) -> ModuleRouter {
    ModuleRouter {
        model: model::Handler::with_ports(
            Arc::new(Ports),
            None,
            model::TurnContext {
                session_id: identity.session_id.clone(),
                turn_id: identity.turn_id.clone(),
                ..Default::default()
            },
        ),
        capability: capability::Handler::new(Arc::new(Ports), Arc::new(Ports), Arc::new(Ports)),
        ..Default::default()
    }
}
fn options() -> InteractionTurnInput {
    InteractionTurnInput {
        permission: PermissionTurnOptions {
            // Deliberately false input: assembly must overwrite from the lease.
            profile: RuntimeProfile::resolve(
                &JsValue::Undefined,
                &RuntimeProfileOverrides {
                    interaction_profile_override: Some(InteractionProfileName::Disabled),
                    ..Default::default()
                },
            )
            .interaction,
            context: permission::create_default_permission_context(
                DefaultPermissionContextOptions {
                    cwd: "/workspace".into(),
                    can_prompt: Some(true),
                    ..Default::default()
                },
            ),
            rules: crate::config::permission::RuleSetHandle::empty(),
            coordinator: Arc::default(),
            emit: Arc::new(|_| true),
            timeout: Duration::from_millis(10),
        },
        hooks: vec![],
        contribution: None,
        base_env: None,
        signal: Cancellation::new(),
        steer_recorder: Some(Arc::new(Ports)),
        record_message: Some(Arc::new(|_| Box::pin(async { Ok(()) }))),
        messages: vec![],
        switches: Default::default(),
        turn_ports: Default::default(),
    }
}
fn identity() -> ExecutionIdentity {
    ExecutionIdentity {
        session_id: "session".into(),
        turn_id: "turn".into(),
        run_id: "run".into(),
        operation_id: "op".into(),
        request_id: "req".into(),
    }
}
fn wire_options() -> JsValue {
    wire_options_for("turn")
}
fn wire_options_for(turn: &str) -> JsValue {
    crate::metadata::object([
        ("sessionId", "session".into()),
        ("turnId", turn.into()),
        ("runId", "run".into()),
    ])
}
async fn client(mode: &str) -> Result<HostClient, HostError> {
    client_with_close_timeout(mode, Limits::default().close_timeout).await
}
async fn client_with_close_timeout(
    mode: &str,
    close_timeout: Duration,
) -> Result<HostClient, HostError> {
    let mut command = Command::new("python3");
    command
        .arg(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/live-session/peer.py"
        ))
        .arg(mode);
    HostClient::spawn(
        &mut command,
        Some(&"fixture".into()),
        Limits {
            execution_timeout: Some(Duration::from_secs(5)),
            close_timeout,
            ..Default::default()
        },
    )
    .await
}
fn adapter(registry: Arc<ProjectRuntimeRegistry>, mode: &'static str) -> LiveSessionAdapter {
    adapter_with_recorder(registry, mode, test_recorder())
}
fn adapter_with_recorder(
    registry: Arc<ProjectRuntimeRegistry>,
    mode: &'static str,
    recorder: Arc<crate::storage::recorder::Recorder>,
) -> LiveSessionAdapter {
    LiveSessionAdapter::with_interaction(recorder, Vec::new, move |identity, _| {
        let lease = registry.acquire(None).expect("#741 notice factory lease");
        Box::pin(async move {
            Ok(InteractionHost {
                client: Box::pin(client(mode)),
                lease,
                router: router(&identity),
                input: options(),
            })
        })
    })
}
fn assert_accepted_once(recorder: &crate::storage::recorder::Recorder, turn: &str, count: usize) {
    let bytes = std::fs::read_to_string(recorder.writer().path()).unwrap();
    let accepted: Vec<_> = bytes
        .lines()
        .map(|line| parse_json(line).unwrap())
        .filter(|row| row.get("type") == Some(&"accepted_input".into()))
        .collect();
    assert_eq!(
        accepted.len(),
        count,
        "every submit must persist accepted_input"
    );
    assert_eq!(
        accepted
            .iter()
            .filter(|row| row.get("turnId") == Some(&turn.into()))
            .count(),
        1,
        "unique turn must reach RecordingAction::Accept"
    );
}

fn host_modules(manifest: &JsObject) -> &JsObject {
    manifest.get("hostModules").unwrap().as_object().unwrap()
}

#[tokio::test]
async fn host_consumes_actual_generations_across_reload_and_live_stdio() {
    let scratch = Scratch::new();
    let registry = Arc::new(interaction_registry(&scratch));
    let old = registry.resolve(None).unwrap();
    let old_events = event_resource(&old.resources().get::<ProjectResources>().unwrap().context());
    let session = adapter(registry.clone(), "live-manifest");
    let mut stream = session
        .submit(
            parse_json(r#"{"type":"blocks","content":[]}"#).unwrap(),
            parse_json(r#"{"sessionId":"session","turnId":"turn","runId":"run","hostModules":{"forged":true}}"#).unwrap(),
        )
        .unwrap();
    assert_eq!(
        bounded(stream.next()).await.unwrap().unwrap().get("type"),
        Some(&"turn_started".into())
    );
    bounded(registry.reload()).await.unwrap();
    assert_eq!(old.state(), RuntimeState::Retired);
    assert!(!old_events.is_closed());
    let current = registry.resolve(None).unwrap();
    assert!(!Arc::ptr_eq(&old, &current));
    let recorder = test_recorder();
    let second = adapter_with_recorder(registry.clone(), "live-manifest", recorder.clone());
    let mut predecessor = second
        .submit(
            parse_json(r#"{"type":"blocks","content":[]}"#).unwrap(),
            wire_options(),
        )
        .unwrap();
    while bounded(predecessor.next()).await.unwrap().is_some() {}
    assert_accepted_once(&recorder, "turn", 1);
    let mut newer = second
        .submit(
            parse_json(r#"{"type":"blocks","content":[]}"#).unwrap(),
            wire_options_for("reload-next"),
        )
        .unwrap();
    bounded(newer.next()).await.unwrap();
    assert_eq!(old.lease_count(), 1);
    assert_eq!(current.lease_count(), 1);
    for running in [&mut stream, &mut newer] {
        bounded(running.next()).await.unwrap().unwrap(); // host accepted
        assert_eq!(
            bounded(running.next()).await.unwrap().unwrap().get("type"),
            Some(&"user_prompt_submitted".into())
        );
        assert_eq!(
            bounded(running.next()).await.unwrap().unwrap().get("type"),
            Some(&"model_request_started".into())
        );
        let observed = bounded(running.next()).await.unwrap().unwrap();
        assert_eq!(observed.get("type"), Some(&"agent_status".into()));
        let actual = observed
            .get("receivedPayload")
            .unwrap()
            .get("hostModules")
            .unwrap();
        for module in ["event", "permission", "lifecycle", "turn"] {
            assert!(actual.get(module).is_some(), "{module}: {actual:?}");
        }
        assert!(actual.get("forged").is_none());
        assert_eq!(
            bounded(running.next()).await.unwrap().unwrap().get("type"),
            Some(&"turn_completed".into())
        );
        assert!(bounded(running.next()).await.unwrap().is_none());
    }
    assert_accepted_once(&recorder, "reload-next", 2);
    bounded(old.wait_disposed()).await.unwrap();
    assert_eq!(old_events.release_count(), 1);
    assert_eq!(current.state(), RuntimeState::Active);
    bounded(registry.dispose()).await.unwrap();
}

#[tokio::test]
async fn host_manifest_uses_each_resource_not_marker_and_preserves_native_checks() {
    // The real registry leases these deliberately partial published bundles.
    for missing in [
        None,
        Some(Resource::InteractionEvents),
        Some(Resource::InteractionPermission),
        Some(Resource::InteractionHooks),
        Some(Resource::InteractionSteer),
        Some(Resource::Interaction),
        Some(Resource::ExecutionWorld),
    ] {
        let registry = ProjectRuntimeRegistry::new(
            PathBuf::from("/workspace"),
            Arc::new(
                move |_: &Path, resources: &mut super::super::ResourceBundle| {
                    let group = interaction_test_group();
                    if missing != Some(Resource::ExecutionWorld) {
                        Families::unwired()
                            .with_tools_mcp()
                            .with_interaction()
                            .execution_world
                            .stage(&group.context(), &mut group.sink())?;
                        let mut slots = group.slots.lock().unwrap();
                        match missing {
                            Some(Resource::Interaction) => slots.interaction = None,
                            Some(Resource::InteractionEvents) => slots.interaction_events = None,
                            Some(Resource::InteractionPermission) => {
                                slots.interaction_permission = None
                            }
                            Some(Resource::InteractionHooks) => slots.interaction_hooks = None,
                            Some(Resource::InteractionSteer) => slots.interaction_steer = None,
                            _ => {}
                        }
                    }
                    resources.add(group);
                    Ok(())
                },
            ),
        );
        let mut reused = router(&identity());
        reused.turn.ports = reused.turn.ports.with_persist_compaction(Arc::new(|_, _| {
            panic!("old generation compaction port must never run")
        }));
        assert_eq!(reused.turn.methods(), Some(vec!["persist_compaction"]));
        let turn = InteractionTurn::bind(
            registry.acquire(None).unwrap(),
            &identity(),
            reused,
            options(),
        )
        .await
        .unwrap();
        for (resource, module) in [
            (Resource::InteractionEvents, "event"),
            (Resource::InteractionPermission, "permission"),
            (Resource::InteractionHooks, "lifecycle"),
            (Resource::InteractionSteer, "turn"),
        ] {
            assert_eq!(
                host_modules(turn.manifest()).contains_key(module),
                missing != Some(resource) && missing != Some(Resource::ExecutionWorld)
            );
        }
        let stale_compaction = turn.module_call(parse_json(
            r#"{"module":"turn","payload":{"operation":"persist_compaction","boundary":{},"messages":[]}}"#,
        ).unwrap()).await;
        assert!(
            stale_compaction.is_err(),
            "stale compaction must not remain callable"
        );
        if let Some(permission) = turn.ports().permission {
            use crate::host::modules::permission::{PermissionContext, PermissionDecision};
            let context = permission
                .prepare(JsValue::Object(JsObject::new()))
                .await
                .unwrap();
            let tool = capability::ToolCatalog::list(&Ports)
                .await
                .unwrap()
                .remove(0);
            for (command, expected) in [("rm -rf /", "deny"), ("sudo echo x", "ask")] {
                let mut input = JsObject::new();
                input.insert("command".into(), command.into());
                let decision = permission
                    .decide(
                        tool.clone(),
                        JsValue::Object(input),
                        context.clone(),
                        "call".into(),
                    )
                    .await
                    .unwrap();
                assert_eq!(
                    decision.get("type"),
                    Some(&expected.into()),
                    "native checks and leased interactive profile must win: {decision:?}"
                );
            }
        }
        bounded(turn.dispose()).await.unwrap();
        bounded(registry.dispose()).await.unwrap();
    }
}

#[tokio::test]
async fn host_explicit_close_and_drop_reject_old_handles_and_release_once() {
    let scratch = Scratch::new();
    let registry = interaction_registry(&scratch);
    let old = registry.resolve(None).unwrap();
    let turn = InteractionTurn::bind(
        registry.acquire(None).unwrap(),
        &identity(),
        router(&identity()),
        options(),
    )
    .await
    .unwrap();
    let ports = turn.ports();
    let hooks = ports.hooks.unwrap();
    let steer = ports.steer.unwrap();
    bounded(registry.reload()).await.unwrap();
    bounded(turn.dispose()).await.unwrap();
    bounded(turn.dispose()).await.unwrap();
    assert!(ports.events.unwrap().drain().is_err());
    assert!(hooks.pending_receipts().is_err());
    assert_eq!(hooks.release_count(), 1);
    assert_eq!(steer.release_count(), 1);
    let replacement = InteractionTurn::bind(
        registry.acquire(None).unwrap(),
        &identity(),
        router(&identity()),
        options(),
    )
    .await
    .unwrap();
    let current = registry.resolve(None).unwrap();
    assert!(steer.cancel("stale".into()).await.is_err());
    assert_eq!(old.state(), RuntimeState::Disposed);
    let done = replacement.wait_closed();
    drop(replacement);
    bounded(done).await.unwrap();
    assert_eq!(current.lease_count(), 0);
    bounded(registry.dispose()).await.unwrap();
}

#[tokio::test]
async fn host_missing_bounded_recorder_rolls_back_all_bound_ports() {
    let scratch = Scratch::new();
    let registry = interaction_registry(&scratch);
    let runtime = registry.resolve(None).unwrap();
    for missing_message in [false, true] {
        let mut input = options();
        let expected = if missing_message {
            input.record_message = None;
            "bounded durable message recorder"
        } else {
            input.steer_recorder = None;
            "bounded I/O"
        };
        let error = InteractionTurn::bind(
            registry.acquire(None).unwrap(),
            &identity(),
            router(&identity()),
            input,
        )
        .await
        .err()
        .unwrap();
        assert!(error.message.contains(expected), "{error:?}");
        until(|| runtime.lease_count() == 0).await;
        assert_eq!(runtime.lease_count(), 0);
        let next = InteractionTurn::bind(
            registry.acquire(None).unwrap(),
            &identity(),
            router(&identity()),
            options(),
        )
        .await
        .unwrap();
        bounded(next.dispose()).await.unwrap();
    }
    bounded(registry.dispose()).await.unwrap();
}

#[tokio::test]
async fn host_live_stdio_dispatches_all_four_ports_and_delivers_buffered_events() {
    let scratch = Scratch::new();
    let registry = Arc::new(interaction_registry(&scratch));
    let recorder = test_recorder();
    let session = adapter_with_recorder(registry.clone(), "live-interaction", recorder.clone());
    for (index, turn_id) in ["turn", "ports-next"].into_iter().enumerate() {
        let mut stream = session
            .submit(
                parse_json(r#"{"type":"blocks","content":[]}"#).unwrap(),
                wire_options_for(turn_id),
            )
            .unwrap();
        let mut events = Vec::new();
        while let Some(event) = bounded(stream.next()).await.unwrap() {
            events.push(event);
        }
        assert_eq!(events[2].get("type"), Some(&"user_prompt_submitted".into()));
        assert_eq!(events.len(), 6, "{events:?}");
        assert_eq!(events[3].get("event"), Some(&"host-probe".into()));
        assert_eq!(events[4].get("event"), Some(&"ports-tested".into()));
        assert_eq!(events[5].get("type"), Some(&"turn_completed".into()));
        assert_accepted_once(&recorder, turn_id, index + 1);
    }
    bounded(registry.dispose()).await.unwrap();
}

fn interaction_registry(scratch: &Scratch) -> ProjectRuntimeRegistry {
    registry_for_yaml(
        scratch,
        CONFIG_YAML_NO_MEMORY,
        silent_families().with_interaction().with_context_memory(),
    )
}

#[tokio::test]
async fn host_manifest_preserves_ts_property_order_and_live_tools() {
    let scratch = Scratch::new();
    let registry = interaction_registry(&scratch);
    let turn = InteractionTurn::bind(
        registry.acquire(None).unwrap(),
        &identity(),
        router(&identity()),
        options(),
    )
    .await
    .unwrap();
    assert_eq!(
        turn.manifest()
            .entries()
            .iter()
            .map(|(key, _)| key.to_utf8().unwrap())
            .collect::<Vec<_>>(),
        [
            "tools",
            "permissionContext",
            "hostModules",
            "interactionCapabilities"
        ],
    );
    let tools = capability::ToolCatalog::list(&Ports).await.unwrap();
    assert_eq!(
        turn.manifest().get("tools"),
        Some(&JsValue::Array(
            tools
                .iter()
                .map(|tool| JsValue::Object(capability::serialize_tool_descriptor(tool.as_ref())))
                .collect()
        ))
    );
    bounded(turn.dispose()).await.unwrap();
    bounded(registry.dispose()).await.unwrap();
}

#[tokio::test]
async fn host_drop_stream_immediately_resubmits_same_session_before_old_dispose() {
    let scratch = Scratch::new();
    let registry = Arc::new(interaction_registry(&scratch));
    let runtime = registry.resolve(None).unwrap();
    let entered = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let mut first = client("live-payload").await.unwrap();
    first.pause_dispose_for_test(entered.clone(), release.clone());
    let clients = Mutex::new(std::collections::VecDeque::from([
        first,
        client("live-payload").await.unwrap(),
    ]));
    let owner = registry.clone();
    let session =
        LiveSessionAdapter::with_interaction(test_recorder(), Vec::new, move |identity, _| {
            let client = clients.lock().unwrap().pop_front().unwrap();
            let lease = owner.acquire(None).unwrap();
            Box::pin(async move {
                Ok(InteractionHost {
                    client: Box::pin(async move { Ok(client) }),
                    lease,
                    router: router(&identity),
                    input: options(),
                })
            })
        });
    let mut first = session
        .submit(
            parse_json(r#"{"type":"blocks","content":[]}"#).unwrap(),
            wire_options(),
        )
        .unwrap();
    bounded(first.next()).await.unwrap().unwrap(); // host started
    bounded(first.next()).await.unwrap().unwrap(); // host accepted
    assert_eq!(
        bounded(first.next()).await.unwrap().unwrap().get("type"),
        Some(&"user_prompt_submitted".into())
    );
    assert_eq!(
        bounded(first.next()).await.unwrap().unwrap().get("type"),
        Some(&"model_request_started".into())
    );
    {
        let mut pending = first.next();
        bounded(async {
            tokio::select! {
                _ = entered.notified() => {},
                result = &mut pending => panic!("disposer must pause before terminal: {result:?}"),
            }
        })
        .await;
    }
    drop(first);
    println!(
        "drop sequence: old disposer paused before InteractionTurn::close; leases={}",
        runtime.lease_count()
    );
    let mut next = session
        .submit(
            parse_json(r#"{"type":"blocks","content":[]}"#).unwrap(),
            wire_options_for("next"),
        )
        .unwrap();
    let event = bounded(next.next()).await.unwrap().unwrap();
    let mut kinds = vec![event.get("type").cloned().unwrap()];
    assert_eq!(
        runtime.lease_count(),
        2,
        "replacement and detached disposer own separate leases"
    );
    release.notify_one();
    until(|| runtime.lease_count() == 1).await;
    // The old disposer runs while the replacement is still active. Its stale
    // handles must not close any of the new turn's interaction resources.
    while let Some(event) = bounded(next.next()).await.unwrap() {
        kinds.push(event.get("type").cloned().unwrap());
    }
    assert_eq!(
        kinds,
        vec![
            "turn_started".into(),
            "input_accepted".into(),
            "user_prompt_submitted".into(),
            "model_request_started".into(),
            "turn_completed".into()
        ]
    );
    assert_eq!(runtime.lease_count(), 0);
    println!(
        "drop sequence: same-session replacement completed; old disposer released exactly once"
    );
    bounded(registry.dispose()).await.unwrap();
}

#[derive(Default)]
struct CleanupRecorder {
    rows: Arc<Mutex<Vec<String>>>,
    hold_discard: std::sync::atomic::AtomicBool,
    fail_discard: bool,
    entered: Arc<tokio::sync::Notify>,
    release: Arc<tokio::sync::Notify>,
}
impl MutationRecorder for CleanupRecorder {
    fn record(&self, _: JsString, mutation: JsObject) -> HostFuture<()> {
        let kind = mutation
            .get("mutation")
            .unwrap()
            .as_string()
            .unwrap()
            .to_utf8()
            .unwrap();
        self.rows.lock().unwrap().push(format!("{kind}:start"));
        let hold = kind == "discard" && self.hold_discard.swap(false, Ordering::SeqCst);
        let fail = kind == "discard" && self.fail_discard;
        let rows = self.rows.clone();
        let entered = self.entered.clone();
        let release = self.release.clone();
        Box::pin(async move {
            if hold {
                entered.notify_one();
                bounded(release.notified()).await;
            }
            rows.lock().unwrap().push(format!("{kind}:end"));
            if fail {
                Err(crate::host::ModuleError::new("injected discard failure").into())
            } else {
                Ok(())
            }
        })
    }
}
fn pending_steer(id: &str) -> crate::runtime::steer::SteerMessage {
    crate::runtime::steer::SteerMessage {
        item_id: id.into(),
        message: parse_json(r#"{"role":"user","content":[]}"#).unwrap(),
        allowed_read_files: None,
    }
}

#[tokio::test]
async fn host_dispose_timeout_resubmit_waits_for_old_recorder() {
    let scratch = Scratch::new();
    let registry = Arc::new(interaction_registry(&scratch));
    let recorder = Arc::new(CleanupRecorder::default());
    recorder.hold_discard.store(true, Ordering::SeqCst);
    // All handshakes finish before the critical poll: Pending cannot be
    // mistaken for client creation instead of the session cleanup barrier.
    let clients = Mutex::new(std::collections::VecDeque::from([
        client_with_close_timeout("live-payload", Duration::from_millis(10))
            .await
            .unwrap(),
        client("live-payload").await.unwrap(),
        client("live-payload").await.unwrap(),
    ]));
    let turns = Arc::new(Mutex::new(Vec::new()));
    let bound = turns.clone();
    let records = recorder.clone();
    let owner = registry.clone();
    let session = LiveSessionAdapter::new(test_recorder(), Vec::new, move |_| {
        let client = clients.lock().unwrap().pop_front().unwrap();
        let lease = Arc::new(owner.acquire(None).unwrap());
        let mut turn_identity = identity();
        if !bound.lock().unwrap().is_empty() {
            turn_identity.turn_id = "next".into();
        }
        let bound = bound.clone();
        let mut input = options();
        input.steer_recorder = Some(records.clone());
        Box::pin(async move {
            let turn =
                InteractionTurn::bind(lease.clone(), &turn_identity, router(&turn_identity), input)
                    .await?;
            bound.lock().unwrap().push(turn.ports().steer.unwrap());
            Ok((
                Box::pin(async move { Ok(client) }) as HostFuture<HostClient>,
                Arc::new(turn) as Arc<dyn HostHandler>,
                lease,
            ))
        })
    });
    let mut first = session
        .submit(
            parse_json(r#"{"type":"blocks","content":[]}"#).unwrap(),
            wire_options(),
        )
        .unwrap();
    bounded(first.next()).await.unwrap().unwrap();
    let old = turns.lock().unwrap()[0].clone();
    old.enqueue(pending_steer("old")).await.unwrap();
    // Terminal delivery and EOF prove HostClient's actual dispose timeout has
    // expired while the old recorder remains deliberately blocked.
    while bounded(first.next()).await.unwrap().is_some() {}
    bounded(recorder.entered.notified()).await;
    println!(
        "timeout sequence: old stream reached EOF with discard held; rows={:?}",
        recorder.rows.lock().unwrap()
    );
    let mut next = session
        .submit(
            parse_json(r#"{"type":"blocks","content":[]}"#).unwrap(),
            wire_options_for("next"),
        )
        .unwrap();
    let mut next_event = next.next();
    std::future::poll_fn(|cx| {
        assert!(next_event.as_mut().poll(cx).is_pending());
        std::task::Poll::Ready(())
    })
    .await;
    let premature = turns.lock().unwrap().get(1).cloned();
    if let Some(turn) = premature {
        turn.enqueue(pending_steer("premature")).await.unwrap();
    }
    println!(
        "timeout sequence: replacement admission observed; rows={:?}",
        recorder.rows.lock().unwrap()
    );
    assert_eq!(
        *recorder.rows.lock().unwrap(),
        ["insert:start", "insert:end", "discard:start"],
        "new recorder write overtook the old discard"
    );
    assert_eq!(
        turns.lock().unwrap().len(),
        1,
        "replacement started before old cleanup completed"
    );
    drop(next_event);
    drop(next); // cancel the partially assembled replacement, including its lease
    let mut next = session
        .submit(
            parse_json(r#"{"type":"blocks","content":[]}"#).unwrap(),
            wire_options_for("next"),
        )
        .unwrap();
    let mut next_event = next.next();
    std::future::poll_fn(|cx| {
        assert!(next_event.as_mut().poll(cx).is_pending());
        std::task::Poll::Ready(())
    })
    .await;
    assert_eq!(
        turns.lock().unwrap().len(),
        1,
        "dropping a waiting binding lost its predecessor barrier"
    );
    let runtime = registry.resolve(None).unwrap();
    until(|| runtime.lease_count() == 2).await; // A and C, not abandoned B
    recorder.release.notify_one();
    bounded(next_event).await.unwrap().unwrap();
    let current = turns.lock().unwrap()[1].clone();
    current.enqueue(pending_steer("new")).await.unwrap();
    assert_eq!(
        *recorder.rows.lock().unwrap(),
        [
            "insert:start",
            "insert:end",
            "discard:start",
            "discard:end",
            "insert:start",
            "insert:end"
        ]
    );
    while bounded(next.next()).await.unwrap().is_some() {}
    assert_eq!(old.release_count(), 1);
    println!(
        "timeout sequence: old discard completed before new insert; rows={:?}",
        recorder.rows.lock().unwrap()
    );
    bounded(registry.dispose()).await.unwrap();
}

#[test]
fn host_interaction_cleanup_runtime_shutdown_is_observable() {
    for close_before_shutdown in [false, true] {
        let executor = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let scratch = Scratch::new();
        let (registry, runtime, turn) = executor.block_on(async {
            let registry = interaction_registry(&scratch);
            let runtime = registry.resolve(None).unwrap();
            let turn = InteractionTurn::bind(
                registry.acquire(None).unwrap(),
                &identity(),
                router(&identity()),
                options(),
            )
            .await
            .unwrap();
            (registry, runtime, turn)
        });
        if close_before_shutdown {
            drop(turn.dispose());
        }
        drop(executor);
        drop(turn.dispose());
        let observer = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        observer.block_on(async {
            let error = bounded(turn.wait_closed()).await.unwrap_err();
            assert!(error.to_string().contains("cleanup task"), "{error:?}");
            assert!(bounded(turn.dispose()).await.unwrap_err().to_string().contains("cleanup task"));
            assert_eq!(runtime.lease_count(), 0);
            println!("stopped runtime: close_before_shutdown={close_before_shutdown}, error={error}, leases=0");
        });
        drop(registry);
    }
}

#[tokio::test]
async fn host_interaction_child_cleanup_failure_is_aggregated_and_releases_lease() {
    let scratch = Scratch::new();
    let registry = interaction_registry(&scratch);
    let runtime = registry.resolve(None).unwrap();
    let mut input = options();
    input.steer_recorder = Some(Arc::new(CleanupRecorder {
        fail_discard: true,
        ..Default::default()
    }));
    let turn = InteractionTurn::bind(
        registry.acquire(None).unwrap(),
        &identity(),
        router(&identity()),
        input,
    )
    .await
    .unwrap();
    let ports = turn.ports();
    ports
        .steer
        .unwrap()
        .enqueue(pending_steer("pending"))
        .await
        .unwrap();
    let error = bounded(turn.dispose()).await.unwrap_err();
    assert!(
        error.to_string().contains("injected discard failure"),
        "{error:?}"
    );
    assert_eq!(
        bounded(turn.wait_closed()).await.unwrap_err().to_string(),
        error.to_string()
    );
    assert_eq!(ports.hooks.unwrap().release_count(), 1);
    assert_eq!(runtime.lease_count(), 0);
    println!("child cleanup: propagated {error}; sibling hooks and lease released");
    assert!(bounded(registry.dispose()).await.is_err());
}

fn command_hook_settings(command: &str) -> crate::hooks::protocol::HooksSettings {
    let mut hook = JsObject::new();
    hook.insert("type".into(), "command".into());
    hook.insert("command".into(), JsValue::String(command.into()));
    let mut matcher = JsObject::new();
    matcher.insert("hooks".into(), JsValue::Array(vec![JsValue::Object(hook)]));
    let mut config = JsObject::new();
    config.insert(
        "PreToolUse".into(),
        JsValue::Array(vec![JsValue::Object(matcher)]),
    );
    crate::hooks::config::parse_hooks_config(&JsValue::Object(config)).settings
}

#[tokio::test]
async fn host_bind_waits_for_old_hook_cleanup_before_returning_replacement() {
    use crate::host::modules::lifecycle;

    let scratch = Scratch::new();
    let registry = Arc::new(interaction_registry(&scratch));
    let lease = registry.acquire(None).unwrap();
    let context = lease
        .runtime()
        .resources()
        .get::<ProjectResources>()
        .unwrap()
        .context();
    let hooks = context
        .resource(Resource::InteractionHooks)
        .unwrap()
        .as_any()
        .downcast_ref::<crate::runtime::project::interaction_hooks::InteractionHooks>()
        .unwrap()
        .clone();
    drop(lease);
    let root = std::env::temp_dir().join(format!("pdrs-693-hooks-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&root).unwrap();
    let command = format!(
        "trap '' TERM; printf started > {0}/started; while [ ! -f {0}/release ]; do sleep 0.01; done; printf done > {0}/done",
        root.display()
    );
    let mut old_router = ModuleRouter::default();
    let _old = hooks
        .bind_turn(
            lifecycle::Binding {
                session_id: "session".into(),
                turn_id: "turn".into(),
                cwd: root.to_string_lossy().to_string().into(),
                permission_mode: Some("default".into()),
                base_env: Some(
                    parse_json(r#"{"PATH":"/usr/bin:/bin"}"#)
                        .unwrap()
                        .as_object()
                        .unwrap()
                        .clone(),
                ),
                signal: None,
            },
            "old-run".into(),
            command_hook_settings(&command),
            None,
            &mut old_router,
        )
        .unwrap();
    let old_call = tokio::spawn(old_router.module_call(parse_json(
        r#"{"module":"lifecycle","payload":{"operation":"dispatch","event":"PreToolUse","payload":{}}}"#,
    ).unwrap()));
    bounded(async {
        while !root.join("started").exists() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await;
    let identity = identity();
    let mut bind = Box::pin(InteractionTurn::bind(
        registry.acquire(None).unwrap(),
        &identity,
        router(&identity),
        options(),
    ));
    assert!(
        tokio::time::timeout(Duration::from_secs(1), &mut bind)
            .await
            .is_err(),
        "InteractionTurn::bind returned before the old hook cleanup barrier"
    );
    assert!(!root.join("done").exists());
    std::fs::write(root.join("release"), b"1").unwrap();
    let turn = tokio::time::timeout(Duration::from_secs(10), bind)
        .await
        .expect("replacement bind did not pass the hook cleanup barrier")
        .unwrap();
    assert!(root.join("done").exists());
    assert!(
        tokio::time::timeout(Duration::from_secs(5), old_call)
            .await
            .expect("old dispatch did not unwind")
            .unwrap()
            .is_err()
    );
    bounded(turn.dispose()).await.unwrap();
    std::fs::remove_dir_all(&root).unwrap();
    bounded(registry.dispose()).await.unwrap();
}

#[test]
fn interaction_cleanup_completion_survives_stopped_executor() {
    for close_before_shutdown in [false, true] {
        let executor = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let scratch = Scratch::new();
        let registry = {
            let _entered = executor.enter();
            interaction_registry(&scratch)
        };
        let mut turn = Some(
            executor
                .block_on(InteractionTurn::bind(
                    registry.acquire(None).unwrap(),
                    &identity(),
                    router(&identity()),
                    options(),
                ))
                .unwrap(),
        );
        let first = turn.as_ref().unwrap().wait_closed();
        let second = turn.as_ref().unwrap().wait_closed();
        if close_before_shutdown {
            drop(turn.take());
        }
        drop(executor);
        if !close_before_shutdown {
            // Cleanup is initiated after the executor is gone; the guard must
            // turn the rejected spawn into the same stable completion.
            drop(turn.take());
        }
        let observer = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        observer.block_on(async {
            for done in [first, second] {
                let error = tokio::time::timeout(std::time::Duration::from_secs(1), done)
                    .await
                    .expect("interaction cleanup completion must survive stopped executor")
                    .unwrap_err();
                assert!(
                    error.to_string().ends_with(
                        "Interaction cleanup task was dropped or panicked before completion."
                    ),
                    "{error}"
                );
            }
        });
        drop(registry);
    }
}

#[tokio::test]
async fn host_next_turn_surfaces_the_pending_hook_discard_notice() {
    live_notice("success").await;
}
#[tokio::test]
async fn host_notice_cancel_after_start() {
    live_notice("cancel").await;
}
#[tokio::test]
async fn host_notice_drop_after_start() {
    live_notice("drop").await;
}
async fn live_notice(mode: &str) {
    let scratch = Scratch::new();
    let registry = Arc::new(interaction_registry(&scratch));
    let identity = identity();
    let mut first_options = options();
    first_options.hooks = command_hook_settings(r#"printf '%s' '{"async":true}'"#);
    first_options.permission.context.cwd =
        std::env::temp_dir().to_string_lossy().to_string().into();
    first_options.base_env = Some(
        parse_json(r#"{"PATH":"/usr/bin:/bin"}"#)
            .expect("#741 notice predecessor setup")
            .as_object()
            .expect("#741 notice predecessor setup")
            .clone(),
    );
    let first = InteractionTurn::bind(
        registry
            .acquire(None)
            .expect("#741 notice predecessor setup"),
        &identity,
        router(&identity),
        first_options,
    )
    .await
    .expect("#741 notice predecessor setup");
    first
        .module_call(parse_json(
            r#"{"module":"lifecycle","payload":{"operation":"dispatch","event":"PreToolUse","payload":{}}}"#,
        ).expect("#741 notice predecessor setup"))
        .await
        .expect("#741 notice predecessor setup");
    assert_eq!(
        first
            .ports()
            .hooks
            .expect("#741 notice predecessor setup")
            .pending_receipts()
            .expect("#741 notice predecessor setup")
            .len(),
        1,
        "#741 notice predecessor pending hook"
    );
    bounded(first.dispose())
        .await
        .expect("#741 notice predecessor setup");

    let session = adapter(registry.clone(), "live-normal");
    let mut stream = session
        .submit(
            crate::metadata::object([("type", "text".into()), ("text", "probe".into())]),
            wire_options(),
        )
        .expect("#741 notice live submit");
    let started = tokio::time::timeout(Duration::from_secs(15), stream.next())
        .await
        .expect("#741 notice first timeout")
        .expect("#741 notice first next")
        .expect("#741 notice first event");
    assert_eq!(
        started.get("type"),
        Some(&"turn_started".into()),
        "#741 notice first start"
    );
    if mode == "drop" {
        drop(stream);
        let mut successor = session
            .submit(
                crate::metadata::object([("type", "text".into()), ("text", "probe".into())]),
                wire_options_for("successor"),
            )
            .expect("#741 notice dropped slot released");
        for kind in ["turn_started", "input_accepted"] {
            let event = tokio::time::timeout(Duration::from_secs(15), successor.next())
                .await
                .expect("#741 notice successor timeout")
                .expect("#741 notice successor next")
                .expect("#741 notice successor event");
            assert_eq!(
                event.get("type"),
                Some(&kind.into()),
                "#741 notice dropped warning not replayed"
            );
        }
        drop(successor);
        bounded(session.dispose())
            .await
            .expect("#741 notice drop dispose");
        bounded(registry.dispose())
            .await
            .expect("#741 notice drop registry dispose");
        return;
    }
    if mode == "cancel" {
        session.abort(&"probe".into()).expect("#741 notice cancel");
    }
    let mut delivered = vec![started];
    for _ in 0..20 {
        let event = tokio::time::timeout(Duration::from_secs(15), stream.next())
            .await
            .expect("#741 notice live next timeout")
            .expect("#741 notice live next");
        let Some(event) = event else { break };
        delivered.push(event);
    }
    let kinds: Vec<_> = delivered.iter().map(|e| e.get("type").cloned()).collect();
    assert!(kinds.len() >= 3, "#741 notice live start order");
    assert_eq!(
        &kinds[..3],
        &[
            Some("turn_started".into()),
            Some("warning".into()),
            Some("input_accepted".into())
        ],
        "#741 notice live start order"
    );
    assert_eq!(
        kinds
            .iter()
            .filter(|k| **k == Some("warning".into()))
            .count(),
        1,
        "#741 notice live warning once"
    );
    assert_eq!(
        kinds
            .iter()
            .filter(|k| **k == Some("turn_started".into()))
            .count(),
        1,
        "#741 notice live start once"
    );
    assert_eq!(
        delivered.get(1),
        Some(&JsValue::Object(
            crate::runtime::project::interaction_notice::DiscardNotice::new(
                crate::runtime::project::interaction_notice::DiscardSource::Hooks,
                1,
                "session".into(),
                "run".into()
            )
            .to_warning(&"session".into(), &"turn".into())
        )),
        "#741 notice live warning payload"
    );
    assert_eq!(
        delivered
            .last()
            .and_then(|e| e.get("result"))
            .and_then(|r| r.get("type")),
        Some(
            &if mode == "cancel" {
                "aborted"
            } else {
                "success"
            }
            .into()
        ),
        "#741 notice live success"
    );
    drop(stream);
    bounded(session.dispose())
        .await
        .expect("#741 notice live dispose");
    bounded(registry.dispose())
        .await
        .expect("#741 notice registry dispose");
}

#[tokio::test]
async fn recording_shared_lease_outlives_handler_cleanup() {
    let scratch = Scratch::new();
    let registry = interaction_registry(&scratch);
    let lease = Arc::new(registry.acquire(None).unwrap());
    let runtime = lease.runtime().clone();
    let turn = InteractionTurn::bind(lease.clone(), &identity(), router(&identity()), options())
        .await
        .unwrap();
    bounded(turn.dispose()).await.unwrap(); // Must not wait for the recorder's Arc.
    registry.invalidate(None);
    assert_eq!(runtime.lease_count(), 1, "shared recorder lease remains");
    drop(lease);
    bounded(runtime.wait_disposed()).await.unwrap();
    assert_eq!(runtime.lease_count(), 0);
}
