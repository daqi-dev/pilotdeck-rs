//! Real registry/lease + turn binding + stdio LiveSession path; no sockets.
#[path = "interaction_fresh_router_tests.rs"]
mod fresh_router;
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
            live::{InteractionHost, LiveSessionAdapter},
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
    parse_json(r#"{"sessionId":"session","turnId":"turn","runId":"run"}"#).unwrap()
}
async fn client(mode: &str) -> Result<HostClient, HostError> {
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
            ..Default::default()
        },
    )
    .await
}
fn adapter(registry: Arc<ProjectRuntimeRegistry>, mode: &'static str) -> LiveSessionAdapter {
    LiveSessionAdapter::with_interaction(move |identity| {
        let lease = registry.acquire(None).unwrap();
        Box::pin(async move {
            Ok(InteractionHost {
                client: client(mode).await?,
                lease,
                router: router(&identity),
                input: options(),
            })
        })
    })
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
    let session = adapter(registry.clone(), "live-payload");
    let mut stream = session
        .submit(
            parse_json(r#"{"messages":[],"hostModules":{"forged":true}}"#).unwrap(),
            wire_options(),
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
    let second = adapter(registry.clone(), "live-payload");
    let mut newer = second
        .submit(parse_json(r#"{"messages":[]}"#).unwrap(), wire_options())
        .unwrap();
    bounded(newer.next()).await.unwrap();
    assert_eq!(old.lease_count(), 1);
    assert_eq!(current.lease_count(), 1);
    for running in [&mut stream, &mut newer] {
        let accepted = bounded(running.next()).await.unwrap().unwrap();
        let actual = accepted
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
    let session = adapter(registry.clone(), "live-interaction");
    let mut stream = session
        .submit(parse_json(r#"{"messages":[]}"#).unwrap(), wire_options())
        .unwrap();
    let mut events = Vec::new();
    while let Some(event) = bounded(stream.next()).await.unwrap() {
        events.push(event);
    }
    assert_eq!(events.len(), 5, "{events:?}");
    assert_eq!(events[2].get("event"), Some(&"host-probe".into()));
    assert_eq!(events[3].get("event"), Some(&"ports-tested".into()));
    assert_eq!(events[4].get("type"), Some(&"turn_completed".into()));
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
