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
fn child_terminal_requires_an_explicit_complete_finish_reason() {
    assert!(terminal_is_complete(
        Some(&"completed".into()),
        Some(&"success".into())
    ));
    for reason in ["max_tokens", "length", "max_turns", "aborted", "error", ""] {
        assert!(
            !terminal_is_complete(Some(&reason.into()), Some(&"success".into())),
            "{reason}"
        );
    }
    assert!(!terminal_is_complete(None, Some(&"success".into())));
    for kind in ["error", "aborted"] {
        assert!(!terminal_is_complete(
            Some(&"completed".into()),
            Some(&kind.into())
        ));
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
