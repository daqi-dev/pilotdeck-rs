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
        file_history: None,
        history_root: None,
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
                permission_mode: None,
                base_permission_mode: None,
                sdk_hooks: None,
                sdk_permission_mode: None,
                sdk_models: None,
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
            permission_mode: None,
            base_permission_mode: None,
            sdk_hooks: None,
            sdk_permission_mode: None,
            sdk_models: None,
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
                permission_mode: None,
                base_permission_mode: None,
                sdk_hooks: None,
                sdk_permission_mode: None,
                sdk_models: None,
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
            permission_mode: None,
            base_permission_mode: None,
            sdk_hooks: None,
            sdk_permission_mode: None,
            sdk_models: None,
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
            permission_mode: None,
            base_permission_mode: None,
            sdk_hooks: None,
            sdk_permission_mode: None,
            sdk_models: None,
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
                permission_mode: None,
                base_permission_mode: None,
                sdk_hooks: None,
                sdk_permission_mode: mode.map(Into::into),
                sdk_models: None,
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

#[test]
fn admitted_mode_reaches_host_permission_and_remembers_plan_base() {
    let (_scratch, tools, recorder, identity) =
        make_tools(JsObject::new(), Arc::default(), RuleSetHandle::empty());
    for (mode, base, expected) in [
        ("bypassPermissions", None, "bypassPermissions"),
        ("plan", Some("bypassPermissions"), "plan"),
        ("default", None, "default"),
    ] {
        let ports = tools.turn_with_authority(
            &identity,
            recorder.clone(),
            Cancellation::new(),
            Some(TurnInteractionAuthority {
                permission_mode: Some(mode.into()),
                base_permission_mode: base.map(Into::into),
                sdk_hooks: None,
                sdk_permission_mode: None,
                sdk_models: None,
                can_prompt: Some(false),
                policy_can_prompt: None,
                emit: Arc::new(|_| false),
            }),
        );
        assert_eq!(
            ports.interaction.permission.context.mode.as_js_string(),
            &JsString::from(expected)
        );
        assert_eq!(
            ports.context_config.get("permissionMode"),
            Some(&JsValue::from(expected))
        );
        assert!(!ports.interaction.permission.context.can_prompt);
        if mode == "plan" {
            assert_eq!(
                tools
                    .permission
                    .0
                    .lock()
                    .unwrap()
                    .1
                    .as_ref()
                    .unwrap()
                    .as_js_string(),
                &JsString::from("bypassPermissions")
            );
        }
    }
}

#[path = "skills_permission_tests.rs"]
mod skills_permission_tests;

#[cfg(unix)]
#[tokio::test]
async fn plan_enter_blocks_a_sibling_write_in_the_same_batch() {
    let (scratch, mut tools, _, identity) =
        make_tools(JsObject::new(), Arc::default(), RuleSetHandle::empty());
    let project = scratch.0.join("project");
    let key = crate::storage::TranscriptKey {
        project_root: project.clone(),
        pilot_home: scratch.0.join("home"),
        session_id: identity.session_id.clone(),
        transcript_path: scratch.0.join("plan-test.jsonl"),
    };
    let storage = crate::runtime::session::factory::FactoryStorage::new(
        Arc::new(crate::storage::JsonlTranscriptStore::new()),
        key,
    );
    let mut started = JsObject::new();
    started.insert("type".into(), "turn_started".into());
    storage
        .recorder
        .writer()
        .record_session_event_checked(&identity.session_id, &identity.turn_id, started)
        .unwrap();
    tools.binding.recorder = storage.recorder.clone();
    tools.binding.projection = storage.projection.clone();
    tools.goal = crate::runtime::goal::GoalRuntime::new(
        identity.session_id.clone(),
        storage.recorder.clone(),
        storage.projection.clone(),
    );
    let recorder = storage.recorder.clone();
    let files = super::super::plan_todo::NativePlanFiles::prepare(project.clone())
        .await
        .unwrap();
    let tools = tools.with_plan_files(Arc::new(files));
    let ports = tools.turn_with_authority(
        &identity,
        recorder,
        Cancellation::new(),
        Some(TurnInteractionAuthority {
            permission_mode: None,
            base_permission_mode: None,
            sdk_hooks: None,
            sdk_permission_mode: None,
            sdk_models: None,
            can_prompt: Some(true),
            policy_can_prompt: None,
            emit: Arc::new(|_| true),
        }),
    );
    let resource = InteractionPermission::default();
    let mut router = crate::host::modules::ModuleRouter::default();
    let catalog: Arc<dyn ToolCatalog> = ports.tool_execution.clone();
    let permission = resource
        .bind_turn(
            identity.session_id.clone(),
            identity.run_id.clone(),
            ports.interaction.permission,
            catalog,
            &mut router,
        )
        .unwrap();
    ports
        .tool_execution
        .bind_turn_permission(permission.clone())
        .unwrap();
    let invocation = ports
        .tool_context
        .prepare(JsValue::Undefined)
        .await
        .unwrap();
    let results = ports
        .tool_execution
        .execute_all(
            vec![
                ToolCall {
                    id: "enter".into(),
                    name: "enter_plan_mode".into(),
                    input: crate::metadata::object([]),
                },
                ToolCall {
                    id: "write".into(),
                    name: "write_file".into(),
                    input: crate::metadata::object([
                        ("file_path", "outside-plan.txt".into()),
                        ("content", "must not be written".into()),
                    ]),
                },
            ],
            invocation,
        )
        .await
        .unwrap();
    assert_eq!(
        results[0].get("type"),
        Some(&"success".into()),
        "{results:?}"
    );
    assert_ne!(
        results[1].get("type"),
        Some(&"success".into()),
        "{results:?}"
    );
    assert!(!project.join("outside-plan.txt").exists());
    permission.close();
}

#[path = "ask_mode_native_tests.rs"]
mod ask_mode_native_tests;

#[cfg(unix)]
#[path = "plan_contract_tests.rs"]
mod plan_contract_tests;

#[path = "native_dialog_tools_tests.rs"]
mod native_dialog_tools_tests;

#[tokio::test]
async fn sdk_progress_reaches_the_real_turn_emitter() {
    use crate::host::modules::tool_context::ToolRuntimeContext;
    let mut config = JsObject::new();
    config.insert("includeToolProgress".into(), JsValue::Bool(true));
    let (_scratch, tools, recorder, identity) =
        make_tools(config, Arc::default(), RuleSetHandle::empty());
    let events = Arc::new(std::sync::Mutex::new(Vec::<JsValue>::new()));
    let captured = events.clone();
    let ports = tools.turn_with_authority(
        &identity,
        recorder,
        Cancellation::new(),
        Some(TurnInteractionAuthority {
            permission_mode: None,
            base_permission_mode: None,
            can_prompt: Some(false),
            policy_can_prompt: None,
            sdk_permission_mode: None,
            sdk_models: None,
            sdk_hooks: None,
            emit: Arc::new(move |value| {
                captured.lock().unwrap().push(value);
                true
            }),
        }),
    );
    let built = ports
        .tool_context
        .prepare(JsValue::Object(JsObject::new()))
        .await
        .unwrap();
    let context = built.runtime.downcast_ref::<ToolRuntimeContext>().unwrap();
    let progress = context
        .progress
        .as_ref()
        .expect("SDK progress must be wired");
    let mut event = JsObject::new();
    for (key, value) in [
        ("toolCallId", "call"),
        ("toolName", "bash"),
        ("message", "stdout: 1 bytes"),
        ("createdAt", "2026-01-01T00:00:00.000Z"),
    ] {
        event.insert(key.into(), value.into());
    }
    progress(event);
    let events = events.lock().unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].get("type"), Some(&"tool_progress".into()));
    assert_eq!(events[0].get("toolCallId"), Some(&"call".into()));
    assert!(
        events[0].get("sessionId").is_none(),
        "gateway projector excludes host identity fields"
    );
}

#[tokio::test]
async fn sdk_tool_catalog_policy_blocks_real_execution_even_in_bypass_mode() {
    for (policy, forbidden, input) in [
        (
            crate::metadata::object([(
                "managedTools",
                crate::metadata::object([
                    ("allow", JsValue::Array(vec!["read_file".into()])),
                    ("deny", JsValue::Array(Vec::new())),
                ]),
            )]),
            "write_file",
            crate::metadata::object([
                ("file_path", "must-not-exist".into()),
                ("content", "FORBIDDEN".into()),
            ]),
        ),
        (
            crate::metadata::object([(
                "sandbox",
                crate::metadata::object([("filesystem", "read_only".into())]),
            )]),
            "write_file",
            crate::metadata::object([
                ("file_path", "must-not-exist".into()),
                ("content", "FORBIDDEN".into()),
            ]),
        ),
        (
            crate::metadata::object([(
                "sandbox",
                crate::metadata::object([("process", "deny".into())]),
            )]),
            "bash",
            crate::metadata::object([("command", "touch must-not-exist".into())]),
        ),
    ] {
        let (_scratch, tools, recorder, identity) = make_tools(
            policy.as_object().unwrap().clone(),
            Arc::default(),
            RuleSetHandle::empty(),
        );
        let target = tools.options.project.join("must-not-exist");
        let tools =
            tools.with_permission_mode_source(Arc::new(|| Some("bypassPermissions".into())));
        let ports = tools.turn(&identity, recorder, Cancellation::new());
        let catalog = ports.tool_execution.list().await.unwrap();
        assert!(
            catalog
                .iter()
                .any(|tool| tool.fields().get("name") == Some(&"read_file".into()))
        );
        assert!(
            !catalog
                .iter()
                .any(|tool| tool.fields().get("name") == Some(&forbidden.into()))
        );
        let resource = InteractionPermission::default();
        let mut router = crate::host::modules::ModuleRouter::default();
        let permission = resource
            .bind_turn(
                identity.session_id,
                identity.run_id,
                ports.interaction.permission,
                ports.tool_execution.clone(),
                &mut router,
            )
            .unwrap();
        ports
            .tool_execution
            .bind_turn_permission(permission.clone())
            .unwrap();
        let context = ports
            .tool_context
            .prepare(JsValue::Undefined)
            .await
            .unwrap();
        let mut results = ports
            .tool_execution
            .execute_all(
                vec![ToolCall {
                    id: "forbidden".into(),
                    name: forbidden.into(),
                    input,
                }],
                context,
            )
            .await
            .unwrap();
        let result = results.remove(0);
        assert_eq!(
            result.get("error").and_then(|error| error.get("code")),
            Some(&"tool_not_found".into())
        );
        assert!(!target.exists());
        let context = ports
            .tool_context
            .prepare(JsValue::Undefined)
            .await
            .unwrap();
        let mut results = ports
            .tool_execution
            .execute_all(
                vec![ToolCall {
                    id: "allowed".into(),
                    name: "read_file".into(),
                    input: crate::metadata::object([("file_path", "probe.txt".into())]),
                }],
                context,
            )
            .await
            .unwrap();
        assert_eq!(results.remove(0).get("type"), Some(&"success".into()));
        ports.tool_execution.dispose().await.unwrap();
        ports.tool_execution.wait_closed().await.unwrap();
        permission.close();
    }
}

#[tokio::test]
async fn sdk_policy_ask_grant_deny_cancel_reaches_real_native_file_execution() {
    for decision in ["allow", "deny", "cancel"] {
        let coordinator = Arc::new(GatewayInteractionCoordinator::default());
        let answerer = coordinator.clone();
        let config = crate::metadata::object([(
            "managedPermissions",
            crate::metadata::object([
                ("deny", JsValue::Array(vec!["Write".into()])),
                ("ask", JsValue::Array(vec!["Read".into()])),
            ]),
        )])
        .as_object()
        .unwrap()
        .clone();
        let base = RuleSetHandle::empty();
        base.update_member("allow", |r| {
            r.push(PermissionRule::new("session", "allow", "write_file").into())
        });
        let policy = super::super::sdk_permission::rules(&config, &base);
        let (scratch, tools, recorder, identity) = make_tools(config, coordinator, policy);
        let tools =
            tools.with_permission_mode_source(Arc::new(|| Some("bypassPermissions".into())));
        let ports = tools.turn_with_authority(
            &identity,
            recorder,
            Cancellation::new(),
            Some(TurnInteractionAuthority {
                permission_mode: None,
                base_permission_mode: None,
                sdk_hooks: None,
                sdk_permission_mode: None,
                sdk_models: None,
                can_prompt: Some(true),
                policy_can_prompt: None,
                emit: Arc::new(move |event| {
                    let event_type = event
                        .get("type")
                        .and_then(JsValue::as_string)
                        .expect("emitted interaction event has a type");
                    if !event_type.equals_str("permission_request") {
                        return true;
                    }

                    answerer.decide_permission(
                        &"session".into(),
                        crate::runtime::interaction::PermissionDecision {
                            request_id: event
                                .get("requestId")
                                .unwrap()
                                .as_string()
                                .unwrap()
                                .clone(),
                            decision: decision.into(),
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
        let permission = resource
            .bind_turn(
                identity.session_id,
                identity.run_id,
                ports.interaction.permission,
                ports.tool_execution.clone(),
                &mut router,
            )
            .unwrap();
        ports
            .tool_execution
            .bind_turn_permission(permission.clone())
            .unwrap();
        let input = crate::metadata::object([("file_path", "probe.txt".into())]);
        let result = router
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
                    ("toolCallId", "sdk-read".into()),
                ]),
            )]))
            .await
            .unwrap();
        assert_eq!(
            result.get("decision").and_then(|v| v.get("type")),
            Some(&"ask".into())
        );
        let answer = permission
            .request(
                "read_file".into(),
                "sdk-read".into(),
                input.clone(),
                vec![],
                Cancellation::new(),
            )
            .await
            .unwrap();
        assert_eq!(
            answer.get("behavior"),
            Some(&if decision == "allow" {
                "allow".into()
            } else {
                "deny".into()
            })
        );
        let invocation = ports
            .tool_context
            .prepare(JsValue::Undefined)
            .await
            .unwrap();
        let results = ports
            .tool_execution
            .execute_all(
                vec![
                    ToolCall {
                        id: "sdk-read".into(),
                        name: "read_file".into(),
                        input,
                    },
                    ToolCall {
                        id: "sdk-write".into(),
                        name: "write_file".into(),
                        input: crate::metadata::object([
                            ("file_path", "denied.txt".into()),
                            ("content", "DENIED".into()),
                        ]),
                    },
                ],
                invocation,
            )
            .await
            .unwrap();
        assert_eq!(
            results[0].get("type") == Some(&"success".into()),
            decision == "allow",
            "{results:?}"
        );
        assert_ne!(results[1].get("type"), Some(&"success".into()));
        assert!(!scratch.0.join("project/denied.txt").exists());
        permission.close();
        ports.tool_execution.dispose().await.unwrap();
        ports.tool_execution.wait_closed().await.unwrap();
    }
}

#[tokio::test]
async fn sdk_additional_root_allows_only_the_declared_real_file() {
    let external =
        Scratch(std::env::temp_dir().join(format!("pdrs-sdk-roots-{}", uuid::Uuid::new_v4())));
    let extra = external.0.join("extra");
    let foreign = external.0.join("foreign");
    std::fs::create_dir_all(&extra).unwrap();
    std::fs::create_dir(&foreign).unwrap();
    std::fs::write(extra.join("ok.txt"), "EXTRA-ROOT").unwrap();
    std::fs::write(foreign.join("no.txt"), "FOREIGN-ROOT").unwrap();
    let config = crate::metadata::object([(
        "additionalWorkingDirectories",
        JsValue::Array(vec![JsValue::String(
            extra.to_string_lossy().into_owned().into(),
        )]),
    )])
    .as_object()
    .unwrap()
    .clone();
    let (_scratch, tools, recorder, identity) =
        make_tools(config, Arc::default(), RuleSetHandle::empty());
    let ports = tools.turn_with_authority(
        &identity,
        recorder,
        Cancellation::new(),
        Some(TurnInteractionAuthority {
            permission_mode: None,
            base_permission_mode: None,
            sdk_hooks: None,
            sdk_permission_mode: None,
            sdk_models: None,
            can_prompt: Some(false),
            policy_can_prompt: None,
            emit: Arc::new(|_| false),
        }),
    );
    let resource = InteractionPermission::default();
    let mut router = crate::host::modules::ModuleRouter::default();
    let permission = resource
        .bind_turn(
            identity.session_id,
            identity.run_id,
            ports.interaction.permission,
            ports.tool_execution.clone(),
            &mut router,
        )
        .unwrap();
    ports
        .tool_execution
        .bind_turn_permission(permission.clone())
        .unwrap();
    let invocation = ports
        .tool_context
        .prepare(JsValue::Undefined)
        .await
        .unwrap();
    let results = ports
        .tool_execution
        .execute_all(
            vec![
                ToolCall {
                    id: "extra".into(),
                    name: "read_file".into(),
                    input: crate::metadata::object([(
                        "file_path",
                        JsValue::String(extra.join("ok.txt").to_string_lossy().into_owned().into()),
                    )]),
                },
                ToolCall {
                    id: "foreign".into(),
                    name: "read_file".into(),
                    input: crate::metadata::object([(
                        "file_path",
                        JsValue::String(
                            foreign.join("no.txt").to_string_lossy().into_owned().into(),
                        ),
                    )]),
                },
            ],
            invocation,
        )
        .await
        .unwrap();
    assert_eq!(
        results[0].get("type"),
        Some(&"success".into()),
        "{results:?}"
    );
    assert_ne!(
        results[1].get("type"),
        Some(&"success".into()),
        "{results:?}"
    );
    permission.close();
    ports.tool_execution.dispose().await.unwrap();
    ports.tool_execution.wait_closed().await.unwrap();
}

#[test]
fn sdk_dontask_uses_default_instead_of_retained_bypass_but_keeps_explicit_turn_mode() {
    let config = crate::metadata::object([("nativeSdkPermissionMode", "dontAsk".into())])
        .as_object()
        .unwrap()
        .clone();
    let (_scratch, tools, recorder, identity) =
        make_tools(config, Arc::default(), RuleSetHandle::empty());
    let tools = tools.with_permission_mode_source(Arc::new(|| Some("bypassPermissions".into())));
    for (explicit, expected) in [(None, "default"), (Some("plan"), "plan")] {
        let ports = tools.turn_with_authority(
            &identity,
            recorder.clone(),
            Cancellation::new(),
            Some(TurnInteractionAuthority {
                permission_mode: explicit.map(Into::into),
                base_permission_mode: None,
                sdk_hooks: None,
                sdk_permission_mode: Some("dontAsk".into()),
                sdk_models: None,
                can_prompt: Some(true),
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
                .equals_str(expected)
        );
        assert!(!ports.interaction.permission.context.can_prompt);
        assert_eq!(
            ports.context_config.get("permissionMode"),
            Some(&expected.into())
        );
    }
}

#[tokio::test]
async fn sdk_ephemeral_turn_spills_under_temp_storage_without_moving_tool_cwd() {
    let sessions = crate::native_process::sdk_ephemeral::EphemeralSessions::default();
    let root = sessions.acquire("session".into()).await.unwrap();
    let config = crate::metadata::object([(
        "nativeEphemeralRoot",
        JsValue::String(root.path().to_string_lossy().into_owned().into()),
    )])
    .as_object()
    .unwrap()
    .clone();
    let (_scratch, tools, recorder, identity) =
        make_tools(config, Arc::default(), RuleSetHandle::empty());
    let project = tools.options.project.clone();
    let ports = tools.turn(&identity, recorder, Cancellation::new());
    assert!(ports.tool_results_dir.starts_with(root.path()));
    assert!(!ports.tool_results_dir.starts_with(&project));
    assert_eq!(
        ports.context_config.get("cwd"),
        Some(&JsValue::String(
            project.to_string_lossy().into_owned().into()
        ))
    );
    let budget = crate::runtime::context::tool_budget::ToolResultBudget::new(
        crate::runtime::context::tool_budget::ToolResultBudgetOptions {
            tool_results_dir: ports.tool_results_dir.clone(),
            max_result_size_chars: Some(1),
            max_result_size_tokens: Some(100),
            preview_bytes: Some(1),
        },
    )
    .unwrap();
    let message = crate::metadata::object([
        ("role", "user".into()),
        (
            "content",
            JsValue::Array(vec![crate::metadata::object([
                ("type", "tool_result".into()),
                ("toolCallId", "spill".into()),
                (
                    "content",
                    JsValue::Array(vec![crate::metadata::object([
                        ("type", "text".into()),
                        ("text", "EPHEMERAL-LARGE-PAYLOAD".into()),
                    ])]),
                ),
            ])]),
        ),
    ]);
    let result = budget
        .apply_to_message(&message, Some(&"turn".into()))
        .await
        .unwrap();
    assert_ne!(result, message);
    assert!(ports.tool_results_dir.exists());
    assert!(!project.join(".pilotdeck/tool-results").exists());
    ports.tool_execution.dispose().await.unwrap();
    ports.tool_execution.wait_closed().await.unwrap();
    sessions.close().await.unwrap();
    assert!(!root.path().exists());
}

#[tokio::test]
async fn native_file_permission_predicates_preserve_registered_reads_explicit_rules_and_accept_edits_bounds()
 {
    for authorization in [
        "registered",
        "session-rule",
        "managed-im",
        "extra-write",
        "foreign-write",
        "foreign-edit",
    ] {
        let external = Scratch(std::env::temp_dir().join(format!(
            "pdrs-file-permission-effect-{}",
            uuid::Uuid::new_v4()
        )));
        std::fs::create_dir_all(&external.0).unwrap();
        let outside = external.0.join("read.txt");
        std::fs::write(&outside, "OUTSIDE-READ").unwrap();
        let rules = RuleSetHandle::empty();
        if authorization == "session-rule" {
            rules.update_member("allow", |r| {
                r.push(PermissionRule::new("session", "allow", "read_file").into())
            });
        }
        let mut config = JsObject::new();
        if authorization == "extra-write" {
            config.insert(
                "additionalWorkingDirectories".into(),
                JsValue::Array(vec![JsValue::String(
                    external.0.to_string_lossy().into_owned().into(),
                )]),
            );
        }
        let (_scratch, tools, recorder, identity) = make_tools(config, Arc::default(), rules);
        let mut path = outside.clone();
        let editing = authorization == "foreign-edit";
        let writing = authorization.ends_with("write") || editing;
        if authorization == "registered" || editing {
            tools
                .binding
                .checkpoint
                .allow_read_files(&[outside.to_string_lossy().into_owned().into()]);
        }
        if authorization == "managed-im" {
            path = tools.options.home.join("im-attachments/managed.txt");
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, "MANAGED-READ").unwrap();
        }
        if writing && !editing {
            path = external.0.join("new-write.txt");
        }
        let ports = tools.turn_with_authority(
            &identity,
            recorder,
            Cancellation::new(),
            Some(TurnInteractionAuthority {
                permission_mode: None,
                base_permission_mode: None,
                sdk_hooks: None,
                sdk_permission_mode: writing.then(|| "acceptEdits".into()),
                sdk_models: None,
                can_prompt: Some(false),
                policy_can_prompt: None,
                emit: Arc::new(|_| false),
            }),
        );
        let resource = InteractionPermission::default();
        let mut router = crate::host::modules::ModuleRouter::default();
        let permission = resource
            .bind_turn(
                identity.session_id,
                identity.run_id,
                ports.interaction.permission,
                ports.tool_execution.clone(),
                &mut router,
            )
            .unwrap();
        ports
            .tool_execution
            .bind_turn_permission(permission.clone())
            .unwrap();
        if editing {
            let reading = ports
                .tool_context
                .prepare(JsValue::Undefined)
                .await
                .unwrap();
            let read = ports
                .tool_execution
                .execute_all(
                    vec![ToolCall {
                        id: "read-before-edit".into(),
                        name: "read_file".into(),
                        input: crate::metadata::object([(
                            "file_path",
                            JsValue::String(path.to_string_lossy().into_owned().into()),
                        )]),
                    }],
                    reading,
                )
                .await
                .unwrap();
            assert_eq!(read[0].get("type"), Some(&"success".into()), "{read:?}");
        }
        let invocation = ports
            .tool_context
            .prepare(JsValue::Undefined)
            .await
            .unwrap();
        let result = ports
            .tool_execution
            .execute_all(
                vec![ToolCall {
                    id: "file-call".into(),
                    name: if editing {
                        "edit_file".into()
                    } else if writing {
                        "write_file".into()
                    } else {
                        "read_file".into()
                    },
                    input: if editing {
                        crate::metadata::object([
                            (
                                "file_path",
                                JsValue::String(path.to_string_lossy().into_owned().into()),
                            ),
                            ("old_string", "OUTSIDE-READ".into()),
                            ("new_string", "MUST-NOT-EDIT".into()),
                        ])
                    } else if writing {
                        crate::metadata::object([
                            (
                                "file_path",
                                JsValue::String(path.to_string_lossy().into_owned().into()),
                            ),
                            ("content", "ACTUAL-WRITE".into()),
                        ])
                    } else {
                        crate::metadata::object([(
                            "file_path",
                            JsValue::String(path.to_string_lossy().into_owned().into()),
                        )])
                    },
                }],
                invocation,
            )
            .await
            .unwrap();
        assert_eq!(
            result[0].get("type") == Some(&"success".into()),
            !authorization.starts_with("foreign-"),
            "{authorization}: {result:?}"
        );
        if editing {
            assert_eq!(std::fs::read_to_string(&path).unwrap(), "OUTSIDE-READ");
            assert_eq!(
                result[0].get("error").and_then(|e| e.get("code")),
                Some(&"permission_required".into()),
                "{result:?}"
            );
        } else if writing {
            assert_eq!(path.exists(), authorization == "extra-write");
            if path.exists() {
                assert_eq!(std::fs::read_to_string(&path).unwrap(), "ACTUAL-WRITE");
            }
        }
        permission.close();
        ports.tool_execution.dispose().await.unwrap();
        ports.tool_execution.wait_closed().await.unwrap();
    }
}

#[test]
fn persisted_rules_reload_each_turn_without_leaking_into_session_or_losing_grants() {
    let base = RuleSetHandle::empty();
    let (_scratch, tools, recorder, identity) =
        make_tools(JsObject::new(), Arc::default(), base.clone());
    let path = tools.options.home.join("permissions.json");
    std::fs::write(&path,r#"{"allowedTools":["Read"],"disallowedTools":["mcp__funasr__transcribe_audio"],"askTools":["Bash(git:*)"]}"#).unwrap();
    let first = tools.turn(&identity, recorder.clone(), Cancellation::new());
    let observed = first.interaction.permission.rules.snapshot();
    assert_eq!(
        observed.allow,
        vec![PermissionRule::new("user", "allow", "read_file")]
    );
    assert_eq!(
        observed.deny,
        vec![PermissionRule::new(
            "user",
            "deny",
            "mcp__funasr__transcribe_audio"
        )]
    );
    assert_eq!(
        observed.ask,
        vec![PermissionRule::new("user", "ask", "bash").with_pattern("git:*")]
    );
    assert!(base.snapshot().allow.is_empty());
    base.update_member("allow", |rules| {
        rules.push(PermissionRule::new("session", "allow", "write_file").into())
    });
    assert_eq!(
        first.interaction.permission.rules.snapshot().allow[0].source,
        JsString::from("session")
    );
    std::fs::write(&path, r#"{"allowedTools":["WebFetch"]}"#).unwrap();
    let second = tools.turn(&identity, recorder, Cancellation::new());
    assert_eq!(
        second.interaction.permission.rules.snapshot().allow,
        vec![
            PermissionRule::new("session", "allow", "write_file"),
            PermissionRule::new("user", "allow", "web_fetch")
        ]
    );
    assert!(
        second
            .interaction
            .permission
            .rules
            .snapshot()
            .deny
            .is_empty()
    );
    assert_eq!(
        first.interaction.permission.rules.snapshot().deny.len(),
        1,
        "previous turn keeps its admitted deny snapshot"
    );
}

#[test]
fn persisted_skip_is_only_fallback_below_managed_session_and_explicit_turn_modes() {
    for (managed, stored, requested, skip, expected) in [
        (None, None, None, true, "bypassPermissions"),
        (None, None, None, false, "default"),
        (None, Some("default"), None, true, "default"),
        (Some("plan"), Some("default"), None, true, "plan"),
        (Some("plan"), None, Some("default"), true, "default"),
        (None, None, Some("plan"), true, "plan"),
    ] {
        let mut config = JsObject::new();
        if let Some(mode) = managed {
            config.insert(
                "managedPermissions".into(),
                crate::metadata::object([("defaultMode", JsValue::from(mode))]),
            );
        }
        let (_scratch, tools, recorder, identity) =
            make_tools(config, Arc::default(), RuleSetHandle::empty());
        let tools = tools.with_permission_mode_source(Arc::new(move || stored.map(JsString::from)));
        std::fs::write(
            tools.options.home.join("permissions.json"),
            format!("{{\"skipPermissions\":{skip}}}"),
        )
        .unwrap();
        let ports = tools.turn_with_authority(
            &identity,
            recorder.clone(),
            Cancellation::new(),
            Some(TurnInteractionAuthority {
                permission_mode: requested.map(JsString::from),
                base_permission_mode: None,
                sdk_hooks: None,
                sdk_permission_mode: None,
                sdk_models: None,
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
                .equals_str(expected)
        );
        if managed.is_none() && stored.is_none() && requested.is_none() && skip {
            std::fs::write(tools.options.home.join("permissions.json"), "{}").unwrap();
            let next = tools.turn(&identity, recorder, Cancellation::new());
            assert!(
                next.interaction
                    .permission
                    .context
                    .mode
                    .as_js_string()
                    .equals_str("default"),
                "persisted bypass must not become a session selection"
            );
        }
    }
}
