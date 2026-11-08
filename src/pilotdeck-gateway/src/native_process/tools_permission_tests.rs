use super::*;
use crate::config::permission::{PermissionRule, RuleSetHandle};
use crate::host::modules::capability::{ToolCall, ToolCatalog};
use crate::runtime::{
    project::interaction_permission::InteractionPermission, session::SessionContext,
    turn::interaction::TurnInteractionAuthority,
};

struct Scratch(PathBuf);
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
fn make_tools(
    mut config: JsObject,
    coordinator: Arc<GatewayInteractionCoordinator>,
    rules: RuleSetHandle,
) -> (Scratch, SessionTools, Arc<Recorder>, ExecutionIdentity) {
    let root =
        std::env::temp_dir().join(format!("pdrs-native-permission-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(root.join("project")).unwrap();
    std::fs::create_dir_all(root.join("home")).unwrap();
    std::fs::write(root.join("project/probe.txt"), "PERMISSION-FIXTURE\n").unwrap();
    let options = Options {
        home: root.join("home"),
        project: root.join("project"),
        listen: "127.0.0.1:0".parse().unwrap(),
        token_file: root.join("unused-token"),
        sidecar: root.join("unused-sidecar"),
        sidecar_args: vec![],
        drain_budget: Duration::from_secs(2),
        storage_root: None,
    };
    let recorder = crate::runtime::turn::live::test_recorder();
    let binding = SessionBinding {
        context: SessionContext {
            session_key: "session".into(),
            project_key: None,
            channel_key: "tui".into(),
            allowed_tools: None,
            disallowed_tools: None,
        },
        recorder: recorder.clone(),
        checkpoint: HostToolCheckpoint::new(None, &[]).unwrap(),
        projection: Arc::default(),
    };
    // Match assembly prepare: cwd is authoritative project input, never a
    // default supplied by ToolContextBuilder or the sidecar.
    config.insert(
        "cwd".into(),
        JsValue::String(options.project.to_string_lossy().into_owned().into()),
    );
    let tools = SessionTools::new(
        &options,
        &crate::config::Env::new(),
        binding,
        config,
        coordinator,
        Arc::default(),
        super::SessionMcpTools::empty(),
    )
    .unwrap()
    .with_permission_rules(rules);
    (
        Scratch(root),
        tools,
        recorder,
        ExecutionIdentity {
            run_id: "run".into(),
            operation_id: "operation".into(),
            request_id: "request".into(),
            session_id: "session".into(),
            turn_id: "turn".into(),
        },
    )
}

#[test]
fn prompt_flags_are_fresh_each_turn_and_managed_false_is_a_ceiling() {
    let rules = RuleSetHandle::empty();
    let (_scratch, tools, recorder, identity) =
        make_tools(JsObject::new(), Arc::default(), rules.clone());
    for (requested, policy, expected) in [
        (Some(false), None, false),
        (None, None, true),
        (Some(true), Some(false), false),
        (None, None, true),
    ] {
        let ports = tools.turn_with_authority(
            &identity,
            recorder.clone(),
            Cancellation::new(),
            Some(TurnInteractionAuthority {
                sdk_permission_mode: None,
                can_prompt: requested,
                policy_can_prompt: policy,
                emit: Arc::new(|_| true),
            }),
        );
        assert_eq!(ports.interaction.permission.context.can_prompt, expected);
        assert_eq!(
            ports
                .context_config
                .get("permissionContext")
                .and_then(|v| v.get("canPrompt")),
            Some(&JsValue::Bool(expected))
        );
        assert_eq!(
            ports.interaction.permission.context.policy_can_prompt,
            policy
        );
        assert!(
            !tools.permission.0.lock().unwrap().0.can_prompt,
            "turn flag leaked into session mode state"
        );
        rules.update_member("deny", |r| {
            r.push(PermissionRule::new("project", "deny", "read_file").into())
        });
        assert!(
            !ports
                .interaction
                .permission
                .rules
                .snapshot()
                .deny
                .is_empty()
        );
    }
    let mut config = JsObject::new();
    config.insert(
        "managedPermissions".into(),
        crate::metadata::object([("canPrompt", JsValue::Bool(false))]),
    );
    let (_scratch, tools, recorder, identity) =
        make_tools(config, Arc::default(), RuleSetHandle::empty());
    let ports = tools.turn_with_authority(
        &identity,
        recorder,
        Cancellation::new(),
        Some(TurnInteractionAuthority {
            sdk_permission_mode: None,
            can_prompt: Some(true),
            policy_can_prompt: None,
            emit: Arc::new(|_| true),
        }),
    );
    assert!(!ports.interaction.permission.context.can_prompt);
    assert_eq!(
        ports.interaction.permission.context.policy_can_prompt,
        Some(false)
    );
}

#[test]
fn selectors_are_read_again_and_managed_default_mode_wins() {
    let (_scratch, tools, recorder, identity) =
        make_tools(JsObject::new(), Arc::default(), RuleSetHandle::empty());
    let selected = Arc::new(Mutex::new(JsString::from("default")));
    let source = selected.clone();
    let tools =
        tools.with_permission_mode_source(Arc::new(move || Some(source.lock().unwrap().clone())));
    for mode in ["default", "plan", "bypassPermissions", "default"] {
        *selected.lock().unwrap() = mode.into();
        let ports = tools.turn_with_authority(
            &identity,
            recorder.clone(),
            Cancellation::new(),
            Some(TurnInteractionAuthority {
                sdk_permission_mode: None,
                can_prompt: None,
                policy_can_prompt: None,
                emit: Arc::new(|_| true),
            }),
        );
        assert!(
            ports
                .interaction
                .permission
                .context
                .mode
                .as_js_string()
                .equals_str(mode)
        );
        assert_eq!(
            ports.context_config.get("permissionMode"),
            Some(&mode.into())
        );
    }
    let mut managed = JsObject::new();
    managed.insert(
        "managedPermissions".into(),
        crate::metadata::object([("defaultMode", "plan".into())]),
    );
    let (_scratch, tools, recorder, identity) =
        make_tools(managed, Arc::default(), RuleSetHandle::empty());
    let tools = tools.with_permission_mode_source(Arc::new(|| Some("bypassPermissions".into())));
    let ports = tools.turn_with_authority(
        &identity,
        recorder,
        Cancellation::new(),
        Some(TurnInteractionAuthority {
            sdk_permission_mode: None,
            can_prompt: None,
            policy_can_prompt: None,
            emit: Arc::new(|_| true),
        }),
    );
    assert!(
        ports
            .interaction
            .permission
            .context
            .mode
            .as_js_string()
            .equals_str("plan")
    );
}

#[tokio::test]
async fn read_file_ask_once_reaches_real_native_execution_and_cannot_be_replayed() {
    let coordinator = Arc::new(GatewayInteractionCoordinator::default());
    let answerer = coordinator.clone();
    let rules = RuleSetHandle::empty();
    rules.update_member("ask", |r| {
        r.push(PermissionRule::new("project", "ask", "read_file").into())
    });
    let (_scratch, tools, recorder, identity) =
        make_tools(JsObject::new(), coordinator.clone(), rules);
    let ports = tools.turn_with_authority(
        &identity,
        recorder,
        Cancellation::new(),
        Some(TurnInteractionAuthority {
            sdk_permission_mode: None,
            can_prompt: None,
            policy_can_prompt: None,
            emit: Arc::new(move |event| {
                answerer.decide_permission(
                    &"session".into(),
                    crate::runtime::interaction::PermissionDecision {
                        request_id: event.get("requestId").unwrap().as_string().unwrap().clone(),
                        decision: "allow".into(),
                        remember: None,
                        reason: None,
                    },
                    None,
                )
            }),
        }),
    );
    let resource = InteractionPermission::default();
    let mut router = crate::host::modules::ModuleRouter::default();
    let catalog: Arc<dyn ToolCatalog> = ports.tool_execution.clone();
    let permission = resource
        .bind_turn(
            identity.session_id,
            identity.run_id,
            ports.interaction.permission,
            catalog,
            &mut router,
        )
        .unwrap();
    ports
        .tool_execution
        .bind_turn_permission(permission.clone())
        .unwrap();
    let input = crate::metadata::object([("file_path", "probe.txt".into())]);
    let decision = router
        .permission
        .call(crate::metadata::object([(
            "payload",
            crate::metadata::object([
                ("operation", "decide".into()),
                (
                    "tool",
                    crate::metadata::object([("name", "read_file".into())]),
                ),
                ("input", input.clone()),
                ("toolCallId", "read-1".into()),
                // A sidecar cannot turn a trusted ask rule into bypass by sending
                // its own permission context.
                (
                    "context",
                    crate::metadata::object([("mode", "bypassPermissions".into())]),
                ),
            ]),
        )]))
        .await
        .unwrap();
    assert_eq!(
        decision.get("decision").and_then(|v| v.get("type")),
        Some(&"ask".into())
    );
    let answer = permission
        .request(
            "read_file".into(),
            "read-1".into(),
            input.clone(),
            vec![],
            Cancellation::new(),
        )
        .await
        .unwrap();
    assert_eq!(answer.get("behavior"), Some(&"allow".into()));
    let invocation = ports
        .tool_context
        .prepare(JsValue::Undefined)
        .await
        .unwrap();
    let result = ports
        .tool_execution
        .execute_all(
            vec![ToolCall {
                id: "read-1".into(),
                name: "read_file".into(),
                input: input.clone(),
            }],
            invocation,
        )
        .await
        .unwrap();
    assert_eq!(result[0].get("type"), Some(&"success".into()));
    assert!(format!("{result:?}").contains("PERMISSION-FIXTURE"));
    let invocation = ports
        .tool_context
        .prepare(JsValue::Undefined)
        .await
        .unwrap();
    let replay = ports
        .tool_execution
        .execute_all(
            vec![ToolCall {
                id: "read-1".into(),
                name: "read_file".into(),
                input,
            }],
            invocation,
        )
        .await
        .unwrap();
    assert_ne!(replay[0].get("type"), Some(&"success".into()));
    assert_eq!(
        coordinator
            .permission_bus()
            .pending_count(&"session".into()),
        0
    );
    permission.close();
}

#[cfg(unix)]
#[path = "native_tools_tests.rs"]
mod native_tools_tests;

#[test]
fn sdk_mode_changes_only_per_turn_permission_context() {
    let (_scratch, tools, recorder, identity) =
        make_tools(JsObject::new(), Arc::default(), RuleSetHandle::empty());
    for (mode, expected_edit, expected_prompt) in [
        (Some("acceptEdits"), Some(true), true),
        (Some("dontAsk"), Some(false), false),
        (None, None, true),
    ] {
        let ports = tools.turn_with_authority(
            &identity,
            recorder.clone(),
            Cancellation::new(),
            Some(TurnInteractionAuthority {
                sdk_permission_mode: mode.map(Into::into),
                can_prompt: Some(true),
                policy_can_prompt: None,
                emit: Arc::new(|_| true),
            }),
        );
        assert_eq!(
            ports.interaction.permission.context.accept_edits,
            expected_edit
        );
        assert_eq!(
            ports.interaction.permission.context.can_prompt,
            expected_prompt
        );
        assert!(
            ports
                .interaction
                .permission
                .context
                .mode
                .as_js_string()
                .equals_str("default")
        );
    }
}
