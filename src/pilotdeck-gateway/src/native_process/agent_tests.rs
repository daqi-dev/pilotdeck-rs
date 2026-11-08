use super::*;
use crate::host::modules::tool_context::{ToolFileState, ToolRuntimeContext};
use std::{future::Future, task::Poll, time::Duration};
fn host_context(turn: Arc<ForkTurn>) -> ToolRunContext {
    let files = ToolFileState {
        read_file_state: Arc::default(),
        write_snapshots: Arc::default(),
        allowed_read_files: vec![],
    };
    let api = ForkApi {
        turn,
        read: files.read_file_state.clone(),
        write: files.write_snapshots.clone(),
    };
    ToolRunContext::new(Arc::new(ToolRuntimeContext {
        fields: JsObject::new(),
        files,
        abort_signal: None,
        model: None,
        elicitation: None,
        user_dialog: None,
        audit_recorder: None,
        file_history: None,
        file_update_notifier: None,
        now: None,
        progress: None,
        subagent: Some(Arc::new(api)),
        plan_todo: None,
        goal: None,
        plan_directory: None,
    }))
}
fn turn(config: JsObject) -> Arc<ForkTurn> {
    ForkTurn::new(
        NativeForkRegistry::new(),
        ExecutionIdentity {
            request_id: "parent-request".into(),
            run_id: "parent-run".into(),
            operation_id: "parent-operation".into(),
            session_id: "parent".into(),
            turn_id: "parent-turn".into(),
        },
        config,
        crate::runtime::turn::live::test_recorder(),
        RuleSetHandle::empty(),
        None,
        None,
        ToolRegistry::new(None, None),
        super::super::skills::NativeSkills::default(),
        None,
        #[cfg(unix)]
        None,
    )
}
fn field<'a>(value: &'a JsValue, name: &str) -> &'a JsValue {
    static ABSENT: JsValue = JsValue::Undefined;
    value.get(name).unwrap_or(&ABSENT)
}
#[tokio::test]
async fn actual_ts_full_fork_public_output_and_pre_fork_rejections_match() {
    let golden = pilotdeck_protocol::parse_json(include_str!("agent_oracle.json")).unwrap();
    let JsValue::Array(cases) = field(&golden, "cases") else {
        panic!("cases")
    };
    let mut outputs = 0;
    let mut rejected = 0;
    for case in cases {
        let id = field(case, "id").as_string().unwrap().to_utf8().unwrap();
        if let Some(data) = field(case, "result").get("data") {
            let description = field(field(case, "input"), "description")
                .as_string()
                .unwrap();
            let definition = field(data, "subagentType")
                .as_string()
                .unwrap()
                .to_utf8()
                .unwrap();
            let report = Report {
                text: field(data, "text").as_string().unwrap().clone(),
                usage: field(data, "usage").clone(),
                turns: field(data, "turns").clone(),
                parsed: field(data, "parsed").as_object().cloned(),
            };
            let output = agent_output(
                &definition,
                description,
                &"child-fixed".into(),
                field(data, "subagentSessionId").as_string().unwrap(),
                &field(data, "transcriptRelativePath")
                    .as_string()
                    .unwrap()
                    .to_utf8()
                    .unwrap(),
                report,
                0.0,
            );
            assert_eq!(
                &object([
                    ("content", JsValue::Array(output.content)),
                    ("data", output.data.unwrap()),
                    ("metadata", JsValue::Object(output.metadata.unwrap()))
                ]),
                field(case, "result"),
                "{id}"
            );
            outputs += 1;
        } else if id == "tool-unknown" || id.starts_with("tool-depth-") && id != "tool-depth-permit"
        {
            let mut config = object([("cwd", "/fixture".into())])
                .as_object()
                .unwrap()
                .clone();
            for (source, target) in [("depth", "subagentDepth"), ("max", "maxSubagentDepth")] {
                if let Some(value) = case.get(source) {
                    config.insert(target.into(), value.clone());
                }
            }
            let turn = turn(config);
            let failure = AgentTool::new()
                .execute(field(case, "input").clone(), host_context(turn.clone()))
                .await
                .unwrap_err();
            let expected = field(case, "error");
            assert_eq!(
                &JsValue::String(failure.code.into()),
                field(expected, "code"),
                "{id}"
            );
            assert_eq!(
                &JsValue::String(failure.message),
                field(expected, "message"),
                "{id}"
            );
            assert_eq!(
                failure.details.map(JsValue::Object),
                expected.get("details").cloned(),
                "{id}"
            );
            assert!(lock(&turn.jobs).is_empty());
            assert_eq!(turn.registry.slots.available_permits(), 8);
            rejected += 1;
        }
    }
    assert_eq!(outputs, 7);
    assert_eq!(rejected, 3);
}
#[test]
fn report_parser_and_type_aliases_keep_raw_utf16_and_missing_fields() {
    for name in ["", "  ", "general_purpose", "general purpose"] {
        assert_eq!(normalize_type(Some(&name.into())), "general-purpose".into());
    }
    assert_eq!(normalize_type(Some(&"Explorer".into())), "explore".into());
    assert!(parse_report(&"not a structured summary".into()).is_none());
    let mut units: Vec<u16> = "Scope: fixture\nResult: ".encode_utf16().collect();
    units.push(0xd800);
    units.extend("\nKey files: none\nFiles changed: none\nIssues: none".encode_utf16());
    assert_eq!(
        parse_report(&JsString::from_utf16(units))
            .unwrap()
            .get("Result"),
        Some(&JsValue::String(JsString::from_utf16(vec![0xd800])))
    );
}
#[tokio::test]
async fn native_fork_event_queue_applies_backpressure_and_close_releases_producer() {
    let turn = turn(JsObject::new());
    for _ in 0..16 {
        turn.emit(
            object([("type", "subagent_model_event".into())])
                .as_object()
                .unwrap()
                .clone(),
        )
        .await
        .unwrap();
    }
    let mut waiting = Box::pin(turn.emit(JsObject::new()));
    assert!(std::future::poll_fn(|cx| Poll::Ready(waiting.as_mut().poll(cx).is_pending())).await);
    lock(&turn.events_rx).try_recv().unwrap();
    tokio::time::timeout(Duration::from_secs(2), waiting)
        .await
        .unwrap()
        .unwrap();
    let mut blocked = Box::pin(turn.emit(JsObject::new()));
    assert!(std::future::poll_fn(|cx| Poll::Ready(blocked.as_mut().poll(cx).is_pending())).await);
    turn.clone().close().await.unwrap();
    assert!(
        tokio::time::timeout(Duration::from_secs(2), blocked)
            .await
            .unwrap()
            .is_err()
    );
}
#[cfg(unix)]
#[test]
fn only_the_real_opaque_fork_and_execute_code_provider_identity_pass_the_sandbox_delegate_gate() {
    use crate::runtime::{project::SandboxMode, tools::sandbox::NativeSandbox};
    let provider = Arc::new(NativeSandbox::new(
        SandboxMode::DangerFullAccess,
        std::env::temp_dir(),
    ));
    let other = Arc::new(NativeSandbox::new(
        SandboxMode::DangerFullAccess,
        std::env::temp_dir(),
    ));
    let mut turn = turn(JsObject::new());
    Arc::get_mut(&mut turn).unwrap().sandbox = Some(provider.clone());
    let context = host_context(turn);
    assert!(AgentTool::new().enforces_native_sandbox(&context, &provider));
    assert!(!AgentTool::new().enforces_native_sandbox(&context, &other));
    let code = crate::runtime::tools::builtin::execute_code::ExecuteCodeTool::with_sandbox(Some(
        provider.clone(),
    ));
    assert!(code.enforces_native_sandbox(&context, &provider));
    assert!(!code.enforces_native_sandbox(&context, &other));
    assert!(
        !crate::runtime::tools::builtin::execute_code::ExecuteCodeTool::new()
            .enforces_native_sandbox(&context, &provider)
    );
}
#[test]
fn session_factory_reference_is_weak_and_does_not_form_an_assembly_cycle() {
    use crate::runtime::session::factory::FactoryOptions;
    let registry = NativeForkRegistry::new();
    let captured = registry.clone();
    let factory = Arc::new(ProjectSessionFactory::new(FactoryOptions {
        writer_busy_wait_timeout: Duration::from_secs(1),
        prepare: Arc::new(move |_, _| {
            let _keep = captured.clone();
            Box::pin(async { Err(RuntimeError::new("unused fixture")) })
        }),
        create_storage: Arc::new(|_| Box::pin(async { Err(RuntimeError::new("unused fixture")) })),
        admission: None,
        diagnostic: Arc::new(|_| {}),
        tool_results_dir: None,
    }));
    registry.install_factory(&factory);
    let weak = Arc::downgrade(&factory);
    drop(factory);
    assert!(weak.upgrade().is_none());
    assert!(registry.factory.get().unwrap().upgrade().is_none());
    assert_eq!(Arc::strong_count(&registry), 1);
}

#[test]
fn actual_ts_max_turns_is_reportable_but_aborted_and_error_are_not() {
    let fixture =
        pilotdeck_protocol::parse_json(include_str!("agent_terminal_oracle.json")).unwrap();
    let terminal = fixture.get("terminal").unwrap();
    let kind = terminal.get("type").and_then(JsValue::as_string);
    assert_eq!(kind, Some(&"max_turns".into()));
    assert_eq!(terminal.get("stopReason"), Some(&"max_turns".into()));
    assert!(terminal_is_reportable(kind));
    let report = fixture.get("report").unwrap();
    assert_eq!(report.get("markdown"), Some(&"".into()));
    assert_eq!(report.get("turns"), Some(&JsValue::Number(2.0)));
    assert_eq!(
        fixture.get("parentCompleted").unwrap().get("errored"),
        Some(&JsValue::Bool(false))
    );
    assert!(terminal_is_reportable(Some(&"success".into())));
    for kind in ["error", "aborted"] {
        assert!(!terminal_is_reportable(Some(&kind.into())));
    }
}

#[tokio::test]
async fn child_close_retains_pending_work_and_returns_sticky_cleanup_failure_to_every_owner() {
    let turn = turn(JsObject::new());
    let (cancelled, receiver) = watch::channel(None);
    let cancel = Cancellation::new();
    lock(&turn.jobs).insert(
        1,
        Job {
            cancel: cancel.clone(),
            done: receiver,
        },
    );
    let mut first = turn.clone().close();
    assert!(cancel.is_aborted());
    assert!(std::future::poll_fn(|cx| Poll::Ready(first.as_mut().poll(cx).is_pending())).await);
    lock(&turn.cleanup_errors).push("gated child storage drain failed".into());
    cancelled.send_replace(Some(Err(error("independent tool error"))));
    let failure = tokio::time::timeout(Duration::from_secs(2), first)
        .await
        .unwrap()
        .unwrap_err();
    assert!(
        failure
            .to_string()
            .contains("gated child storage drain failed")
    );
    let repeated = turn.clone().close().await.unwrap_err();
    assert_eq!(failure.to_string(), repeated.to_string());
}

#[test]
fn sdk_replacement_scope_and_initial_policy_keep_parent_ceiling() {
    let mut config = JsObject::new();
    config.insert(
        "subagentDefinitions".into(),
        object([(
            "fixture",
            object([
                (
                    "allowedTools",
                    JsValue::Array(vec!["read_file".into(), "bash".into()]),
                ),
                ("disallowedTools", JsValue::Array(vec!["bash".into()])),
                ("systemPromptSuffix", "CHILD SUFFIX".into()),
                ("criticalSystemReminder", "CHILD REMINDER".into()),
            ]),
        )]),
    );
    let available = definitions(&config);
    assert!(available.get("general-purpose").is_none());
    let spec = available.get("fixture").unwrap().as_object().unwrap();
    assert!(definition_allows(spec, &"read_file".into()));
    assert!(!definition_allows(spec, &"bash".into()));
    assert!(!definition_allows(spec, &"write_file".into()));
    let prompt = definition_prompt("fixture", spec, Some(&"PARENT".into()))
        .to_utf8()
        .unwrap();
    assert!(prompt.contains("CHILD SUFFIX"));
    assert!(prompt.contains("CHILD REMINDER"));
    assert!(prompt.ends_with("PARENT"));
    config.insert(
        "subagentDefinitions".into(),
        JsValue::Object(JsObject::new()),
    );
    assert!(definitions(&config).entries().is_empty());
}

#[test]
fn sdk_initial_prompt_is_durable_before_directive_and_wire_input_cannot_construct_it() {
    use crate::runtime::turn::runner::{AgentInput, TurnInput};
    let input = TurnInput {
        input: AgentInput::Fork {
            initial: "initial".into(),
            directive: "directive".into(),
        },
        history: vec![],
        synthetic: vec![],
        metadata: None,
    };
    let accepted = input.accepted_messages();
    assert_eq!(accepted.len(), 2);
    assert_eq!(accepted, input.loop_messages());
    let texts: Vec<_> = accepted
        .iter()
        .map(|message| {
            message
                .get("content")
                .and_then(|v| {
                    if let JsValue::Array(items) = v {
                        items.first()
                    } else {
                        None
                    }
                })
                .and_then(|v| v.get("text"))
                .unwrap()
                .clone()
        })
        .collect();
    assert_eq!(texts, vec!["initial".into(), "directive".into()]);
    assert!(
        TurnInput::from_agent_input(
            &object([
                ("type", "fork".into()),
                ("initialPrompt", "untrusted".into())
            ]),
            &JsValue::Object(JsObject::new()),
            vec![],
            vec![]
        )
        .is_err()
    );
}

#[tokio::test]
async fn warm_parent_child_inherits_actual_turn_generation_after_reload() {
    use crate::runtime::project::{ProjectRuntimeRegistry, ResourceBundle, RuntimeState};
    let registry = ProjectRuntimeRegistry::new(
        "/fixture-turn-generation",
        Arc::new(|_: &std::path::Path, _: &mut ResourceBundle| Ok(())),
    );
    let prepare = registry.acquire(None).unwrap();
    let old = prepare.runtime().clone();
    let owner = NativeForkOwner::new(prepare);
    registry.reload().await.unwrap();
    let current = registry.acquire(None).unwrap();
    let new = current.runtime().clone();
    assert!(!Arc::ptr_eq(&old, &new));
    assert_eq!(old.state(), RuntimeState::Retired);
    let turn = turn(JsObject::new());
    assert!(turn.generation.get().is_none());
    turn.bind_generation(Arc::new(current)).unwrap();
    let mut child = turn.generation.get().unwrap().fork().unwrap();
    assert!(Arc::ptr_eq(child.runtime(), &new));
    assert!(!Arc::ptr_eq(child.runtime(), &old));
    assert!(
        turn.bind_generation(owner.generation().unwrap()).is_err(),
        "a later/old source cannot replace this turn's binding"
    );
    child.release().await.unwrap();
    turn.clone().close().await.unwrap();
    drop(turn);
    owner.release_generation().await.unwrap();
    registry.dispose().await.unwrap();
}

#[tokio::test]
async fn subagent_lifecycle_uses_real_hook_runtime_and_retries_rejected_success_stop_as_failure() {
    use crate::{
        hooks::protocol::{HookCommand, HookMatcher},
        host::modules::lifecycle::LifecyclePort,
        runtime::hooks::{
            HookRuntime, LifecycleDispatchInput, LifecycleDispatchResult, LifecycleRuntime,
            exec::{CallbackFuture, CallbackHookHandler, CallbackInput, CallbackResult},
        },
    };
    struct Callback(Arc<Mutex<Vec<JsObject>>>);
    impl CallbackHookHandler for Callback {
        fn call(&self, input: CallbackInput) -> CallbackFuture {
            lock(&self.0).push(input.hook_input);
            Box::pin(async { Ok(CallbackResult::Void) })
        }
    }
    struct Port {
        runtime: LifecycleRuntime,
        seen: Arc<Mutex<Vec<JsObject>>>,
        reject_true: std::sync::atomic::AtomicBool,
    }
    impl LifecyclePort for Port {
        fn dispatch(&self, input: LifecycleDispatchInput) -> HostFuture<LifecycleDispatchResult> {
            lock(&self.seen).push(
                object([
                    ("event", JsValue::String(input.event.clone())),
                    ("base", JsValue::Object(input.base_input.clone())),
                    (
                        "payload",
                        JsValue::Object(input.payload.clone().unwrap_or_default()),
                    ),
                    (
                        "env",
                        JsValue::Object(input.env.clone().unwrap_or_default()),
                    ),
                    (
                        "matchQuery",
                        input
                            .match_query
                            .clone()
                            .map(JsValue::String)
                            .unwrap_or(JsValue::Undefined),
                    ),
                ])
                .as_object()
                .unwrap()
                .clone(),
            );
            if input.event.equals_str("SubagentStop")
                && input
                    .payload
                    .as_ref()
                    .and_then(|payload| payload.get("success"))
                    == Some(&JsValue::Bool(true))
                && self.reject_true.swap(false, Ordering::SeqCst)
            {
                return Box::pin(async {
                    Err(ModuleError::new("fixture successful-stop dispatch rejected").into())
                });
            }
            let runtime = self.runtime.clone();
            Box::pin(async move { runtime.dispatch(input).await })
        }
    }
    let callbacks = Arc::new(Mutex::new(vec![]));
    let settings = ["SubagentStart", "SubagentStop"]
        .into_iter()
        .map(|event| {
            (
                event.into(),
                vec![HookMatcher {
                    hooks: vec![HookCommand::Callback {
                        name: "fixture".into(),
                        condition: None,
                        status_message: None,
                    }],
                    ..Default::default()
                }],
            )
        })
        .collect();
    let runtime = HookRuntime::new(settings);
    let _registration = runtime
        .callback_executor()
        .register(&"fixture".into(), Arc::new(Callback(callbacks.clone())))
        .unwrap();
    let seen = Arc::new(Mutex::new(vec![]));
    let port = Arc::new(Port {
        runtime: LifecycleRuntime::new(runtime),
        seen: seen.clone(),
        reject_true: std::sync::atomic::AtomicBool::new(true),
    });
    let turn = turn(
        object([
            ("cwd", "/fixture-subagent-hook".into()),
            ("permissionMode", "default".into()),
            ("env", JsValue::Object(JsObject::new())),
        ])
        .as_object()
        .unwrap()
        .clone(),
    );
    assert!(turn.lifecycle.set(port).is_ok());
    turn.dispatch_subagent_hook("SubagentStart", &"child".into(), "explore", None)
        .await
        .unwrap();
    let error = turn
        .stop_subagent_hooks(&"child".into(), "explore", true)
        .await
        .unwrap_err();
    assert!(
        error
            .message
            .to_utf8()
            .unwrap()
            .contains("fixture successful-stop")
    );
    let seen = lock(&seen);
    assert_eq!(seen.len(), 3);
    assert_eq!(seen[0].get("event"), Some(&"SubagentStart".into()));
    assert_eq!(
        seen[1].get("payload").unwrap().get("success"),
        Some(&JsValue::Bool(true))
    );
    assert_eq!(
        seen[2].get("payload").unwrap().get("success"),
        Some(&JsValue::Bool(false))
    );
    for input in seen.iter() {
        assert_eq!(
            input.get("base").unwrap().get("sessionId"),
            Some(&"parent".into())
        );
        assert_eq!(
            input.get("base").unwrap().get("transcriptPath"),
            Some(&"".into())
        );
        assert_eq!(
            input.get("env").unwrap().get("PILOTDECK_SESSION_ID"),
            Some(&"parent".into())
        );
        assert_eq!(
            input.get("env").unwrap().get("PILOTDECK_TURN_ID"),
            Some(&"parent-turn".into())
        );
        assert_eq!(input.get("matchQuery"), input.get("event"));
    }
    drop(seen);
    let callbacks = lock(&callbacks);
    assert_eq!(callbacks.len(), 2);
    assert_eq!(
        callbacks[0].get("hookEventName"),
        Some(&"SubagentStart".into())
    );
    assert_eq!(
        callbacks[1].get("hookEventName"),
        Some(&"SubagentStop".into())
    );
    assert_eq!(callbacks[1].get("success"), Some(&JsValue::Bool(false)));
}
