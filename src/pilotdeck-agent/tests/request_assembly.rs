//! Controlled source-derived request-policy tests. These are not formal host,
//! original Node differential, media/FS or live AgentLoop acceptance.
use pilotdeck_agent::{MessageProjection, ProjectedMessage, ProjectionGap, ports, request, turn};
use pilotdeck_protocol::model::{
    BoundarySnapshot, ModelProtocol, ThinkingConfig, ThinkingMode, ToolChoice,
};
use pilotdeck_protocol::{JsObject, JsString, JsValue};
use ports::{
    CallControl, Cancellation, Captured, HostCall, HostPort, HostReply, ModelMetadata,
    PermissionMode, PortError, PortResult, Route, RuntimeContextSurface, TokenLimits,
    ToolDescriptor,
};
use request::{
    RequestAssemblyInput, RequestAssemblyOptions, RequestPreparationObserver,
    RequestPreparationState, RouteTokenCaps,
};
use std::collections::VecDeque;
use turn::{
    RunMode, TurnInput, TurnModelOverride, TurnPermissionContext, TurnRuntimeConfig,
    TurnSubagentModel,
};
#[path = "../src/request_assembly.rs"]
mod request_assembly;

fn obj(fields: impl IntoIterator<Item = (&'static str, JsValue)>) -> JsValue {
    let mut out = JsObject::new();
    for (key, value) in fields {
        out.insert(key.into(), value);
    }
    JsValue::Object(out)
}
fn arr(values: impl IntoIterator<Item = JsValue>) -> JsValue {
    JsValue::Array(values.into_iter().collect())
}
fn n(value: f64) -> JsValue {
    JsValue::Number(value)
}
fn b(value: bool) -> JsValue {
    JsValue::Bool(value)
}
fn route(provider: &str, model: &str) -> Route {
    Route {
        provider: provider.into(),
        model: model.into(),
    }
}
fn block(text: &str) -> JsValue {
    obj([("type", "text".into()), ("text", text.into())])
}
fn msg(role: &str, content: JsValue) -> JsValue {
    obj([("role", role.into()), ("content", content)])
}
fn project(value: JsValue) -> ProjectedMessage {
    ProjectedMessage {
        boundary: BoundarySnapshot::capture(&value),
        normalized: MessageProjection::BoundaryOnly(vec![ProjectionGap::OtherBoundaryShape]),
    }
}
fn array(value: &JsValue) -> &[JsValue] {
    if let JsValue::Array(values) = value {
        values
    } else {
        panic!("expected array")
    }
}
fn config() -> TurnRuntimeConfig {
    TurnRuntimeConfig {
        route: route("provider", "model"),
        cwd: "/controlled/project".into(),
        system_prompt: None,
        append_system_prompt: None,
        plan_mode_instructions: None,
        runtime_context_surface: RuntimeContextSurface::SystemPrompt,
        max_output_tokens: None,
        max_context_tokens: None,
        thinking: None,
        tool_choice: None,
        max_context_messages: None,
        stop_on_structured_output: None,
        json_self_correct: None,
        run_mode: None,
        is_subagent: None,
        subagent_model: None,
        permission_mode_before_plan: None,
        permission: TurnPermissionContext {
            mode: PermissionMode::Default,
            can_prompt: false,
            bypass_available: false,
            additional_working_directories: vec![],
            rules: None,
        },
        metadata: None,
    }
}
fn input() -> TurnInput {
    TurnInput {
        session_id: "session".into(),
        turn_id: "turn".into(),
        messages: vec![],
        max_turns: None,
        max_budget_usd: None,
        task_budget_usd: None,
        initial_task_budget_spent_usd: None,
        base_permission_mode: None,
        allow_plan_mode_tools: false,
        can_elicit: false,
        model_override: None,
    }
}
fn state() -> RequestPreparationState {
    RequestPreparationState {
        tools: vec![],
        metadata: vec![],
        plan_todo_addendum: None,
        elicitation_available: false,
        token_caps: vec![],
    }
}
fn control() -> CallControl {
    CallControl {
        cancellation: Cancellation::default(),
        deadline: None,
    }
}
fn tool(name: &str) -> ToolDescriptor {
    ToolDescriptor {
        name: name.into(),
        description: "source description".into(),
        kind: "custom".into(),
        input_schema: obj([("type", "object".into())])
            .as_object()
            .unwrap()
            .clone(),
        read_only: false,
        concurrency_safe: false,
        requires_user_interaction: false,
        required_runtime_capabilities: Some(vec![]),
    }
}
fn captured_tool(tool: ToolDescriptor) -> Captured<ToolDescriptor> {
    let boundary = obj([
        ("name", JsValue::String(tool.name.clone())),
        ("description", JsValue::String(tool.description.clone())),
        ("kind", JsValue::String(tool.kind.clone())),
        ("inputSchema", JsValue::Object(tool.input_schema.clone())),
        ("readOnly", b(tool.read_only)),
        ("concurrencySafe", b(tool.concurrency_safe)),
        ("requiresUserInteraction", b(tool.requires_user_interaction)),
    ]);
    Captured {
        view: tool,
        boundary: BoundarySnapshot::capture(&boundary),
    }
}
fn prepared(messages: Vec<JsValue>) -> JsValue {
    obj([
        ("messages", arr(messages)),
        ("tools", arr([])),
        ("systemPromptParts", arr([])),
        ("diagnostics", arr([])),
        ("boundaries", arr([])),
    ])
}
fn metadata(
    route: Route,
    protocol: Option<ModelProtocol>,
    supports: Option<bool>,
    output: Option<f64>,
    combined: Option<TokenLimits>,
) -> Captured<ModelMetadata> {
    let mut raw = obj([
        ("provider", JsValue::String(route.provider.clone())),
        ("model", JsValue::String(route.model.clone())),
    ])
    .as_object()
    .unwrap()
    .clone();
    if let Some(output) = output {
        raw.insert("maxOutputTokens".into(), n(output));
    }
    if let Some(supports) = supports {
        raw.insert("supportsPromptCache".into(), b(supports));
    }
    if let Some(protocol) = protocol {
        let name = match protocol {
            ModelProtocol::Anthropic => "anthropic",
            ModelProtocol::OpenAi => "openai",
            ModelProtocol::OpenAiResponses => "openai-responses",
            ModelProtocol::Google => "google",
        };
        raw.insert("protocol".into(), name.into());
    }
    if let Some(limits) = &combined {
        let mut limits_raw = obj([("maxContextTokens", n(limits.max_context_tokens))])
            .as_object()
            .unwrap()
            .clone();
        if let Some(output) = limits.max_output_tokens {
            limits_raw.insert("maxOutputTokens".into(), n(output));
        }
        raw.insert("tokenLimits".into(), JsValue::Object(limits_raw));
    }
    Captured {
        view: ModelMetadata {
            route,
            max_context_tokens: None,
            max_output_tokens: output,
            token_limits: combined,
            protocol,
            supports_prompt_cache: supports,
        },
        boundary: BoundarySnapshot::capture(&JsValue::Object(raw)),
    }
}
fn cap() -> RouteTokenCaps {
    RouteTokenCaps {
        route: route("provider", "model"),
        max_context_tokens: None,
        attempt_max_output_tokens: None,
        requested_max_output_tokens: None,
        session_max_output_tokens: None,
        hard_max_output_tokens: None,
    }
}
#[derive(Default)]
struct Host {
    catalog: bool,
    context: bool,
    calls: Vec<HostCall>,
    replies: VecDeque<PortResult<HostReply>>,
    cancel_after: Option<(usize, Cancellation)>,
}
impl HostPort for Host {
    fn advertised(&self, module: &str, operation: &str) -> bool {
        (module == "capability" && operation == "list_tools" && self.catalog)
            || (module == "context" && operation == "prepare_for_model" && self.context)
    }
    fn call(&mut self, control: &CallControl, call: HostCall) -> PortResult<HostReply> {
        control.check()?;
        assert!(
            matches!(call, HostCall::ListTools | HostCall::PrepareContext(_)),
            "assembler must not emit events/prepare/stream"
        );
        self.calls.push(call);
        if let Some((index, cancellation)) = &self.cancel_after
            && self.calls.len() == *index
        {
            cancellation.cancel(ports::CancelCause::Requested("test".into()));
        }
        self.replies.pop_front().expect("bounded unit host reply")
    }
}
fn context_host(prepared: JsValue) -> Host {
    Host {
        context: true,
        replies: VecDeque::from([Ok(HostReply::ContextBoundaryOnly(
            BoundarySnapshot::capture(&prepared),
        ))]),
        ..Host::default()
    }
}
fn call_input(host: &Host) -> &ports::ContextPrepare {
    match host.calls.last().unwrap() {
        HostCall::PrepareContext(input) => input,
        _ => panic!("expected context call"),
    }
}
#[derive(Default)]
struct Observer {
    notices: Vec<bool>,
    fail: bool,
}
impl RequestPreparationObserver for Observer {
    fn instructions_loaded(&mut self, has: bool) -> PortResult<()> {
        self.notices.push(has);
        if self.fail {
            Err(PortError::TransportBeforeDispatch {
                message: "observer failure".into(),
            })
        } else {
            Ok(())
        }
    }
}
fn assemble(
    s: &mut RequestPreparationState,
    h: &mut Host,
    c: &TurnRuntimeConfig,
    i: &TurnInput,
    m: &[ProjectedMessage],
) -> PortResult<request::PreparedRequestIntent> {
    request_assembly::assemble(s, h, &control(), c, i, m, &mut Observer::default())
}

#[test]
fn catalog_refresh_precedes_context_and_replaces_only_after_success() {
    let mut s = state();
    s.tools = vec![tool("old")];
    let new = tool("new");
    let mut host = Host {
        catalog: true,
        context: true,
        replies: VecDeque::from([
            Ok(HostReply::Tools(vec![captured_tool(new)])),
            Ok(HostReply::ContextBoundaryOnly(BoundarySnapshot::capture(
                &prepared(vec![]),
            ))),
        ]),
        ..Host::default()
    };
    assemble(&mut s, &mut host, &config(), &input(), &[]).unwrap();
    assert!(matches!(host.calls[0], HostCall::ListTools));
    assert!(matches!(host.calls[1], HostCall::PrepareContext(_)));
    assert_eq!(call_input(&host).tools[0].name, JsString::from("new"));
    assert_eq!(s.tools[0].name, JsString::from("new"));
    let mut failed = Host {
        catalog: true,
        replies: VecDeque::from([Err(PortError::TransportBeforeDispatch {
            message: "catalog failure".into(),
        })]),
        ..Host::default()
    };
    assert!(assemble(&mut s, &mut failed, &config(), &input(), &[]).is_err());
    assert_eq!(s.tools[0].name, JsString::from("new"));
    assert_eq!(failed.calls.len(), 1);
}
#[test]
fn ack_abort_keeps_refreshed_catalog_and_makes_no_context_call() {
    let control = control();
    let mut s = state();
    let mut host = Host {
        catalog: true,
        context: true,
        replies: VecDeque::from([Ok(HostReply::Tools(vec![captured_tool(tool("new"))]))]),
        cancel_after: Some((1, control.cancellation.clone())),
        ..Host::default()
    };
    assert!(matches!(
        request_assembly::assemble(
            &mut s,
            &mut host,
            &control,
            &config(),
            &input(),
            &[],
            &mut Observer::default()
        ),
        Err(PortError::Cancelled { .. })
    ));
    assert_eq!(host.calls.len(), 1);
    assert_eq!(s.tools[0].name, JsString::from("new"));
}
#[test]
fn adjacent_assistant_merge_preserves_metadata_overlay_and_discards_top_extensions() {
    let first = obj([
        ("role", "assistant".into()),
        ("content", arr([block("one")])),
        (
            "metadata",
            obj([("first", b(true)), ("shared", "old".into())]),
        ),
        ("extension", "discard first".into()),
    ]);
    let second = obj([
        ("role", "assistant".into()),
        ("content", arr([])),
        (
            "metadata",
            obj([("shared", JsValue::Undefined), ("second", JsValue::Null)]),
        ),
        ("extension", "discard second".into()),
    ]);
    let mut host = context_host(prepared(vec![]));
    assemble(
        &mut state(),
        &mut host,
        &config(),
        &input(),
        &[project(first), project(second)],
    )
    .unwrap();
    let messages = &call_input(&host).messages;
    assert_eq!(messages.len(), 1);
    assert_eq!(
        messages[0].boundary.value(),
        &obj([
            ("role", "assistant".into()),
            ("content", arr([block("one")])),
            (
                "metadata",
                obj([
                    ("first", b(true)),
                    ("shared", JsValue::Undefined),
                    ("second", JsValue::Null)
                ])
            )
        ])
    );
}
#[test]
fn tool_call_prevents_merge_and_own_undefined_input_survives_clone() {
    let mut host = context_host(prepared(vec![]));
    let messages = [
        project(msg(
            "assistant",
            arr([obj([
                ("type", "tool_call".into()),
                ("id", "call".into()),
                ("name", "tool".into()),
                ("raw", JsValue::Null),
            ])]),
        )),
        project(msg("assistant", arr([block("second")]))),
        project(msg("assistant", arr([]))),
    ];
    assemble(&mut state(), &mut host, &config(), &input(), &messages).unwrap();
    let messages = &call_input(&host).messages;
    assert_eq!(messages.len(), 2);
    let first = &array(messages[0].boundary.value().get("content").unwrap())[0];
    assert_eq!(first.get("input"), Some(&JsValue::Undefined));
    assert_eq!(first.get("raw"), Some(&JsValue::Null));
}
#[test]
fn lax_messages_and_primitive_blocks_follow_own_spread_rules() {
    let messages = [
        project(obj([
            ("role", "user".into()),
            ("content", "legacy".into()),
            ("extension", JsValue::Undefined),
        ])),
        project(msg("user", arr(["ab".into(), n(4.0)]))),
    ];
    let mut host = context_host(prepared(vec![msg("user", arr(["xy".into()]))]));
    let result = assemble(&mut state(), &mut host, &config(), &input(), &messages).unwrap();
    let messages = &call_input(&host).messages;
    assert_eq!(messages[0].boundary.value().get("content"), Some(&arr([])));
    assert_eq!(
        messages[0].boundary.value().get("extension"),
        Some(&JsValue::Undefined)
    );
    assert_eq!(
        messages[1].boundary.value().get("content"),
        Some(&arr([obj([("0", "a".into()), ("1", "b".into())]), obj([])]))
    );
    assert_eq!(
        result.request.value().get("messages"),
        Some(&arr([msg(
            "user",
            arr([obj([("0", "x".into()), ("1", "y".into())])])
        )]))
    );
}
#[test]
fn invalid_block_clone_fails_after_catalog_before_context_without_empty_fabrication() {
    let mut host = Host {
        catalog: true,
        context: true,
        replies: VecDeque::from([Ok(HostReply::Tools(vec![captured_tool(tool("new"))]))]),
        ..Host::default()
    };
    let mut s = state();
    let invalid = project(msg(
        "user",
        arr([obj([
            ("type", "tool_result".into()),
            ("content", JsValue::Null),
        ])]),
    ));
    let error = assemble(&mut s, &mut host, &config(), &input(), &[invalid]).unwrap_err();
    assert!(matches!(error, PortError::SourceException { .. }));
    assert_eq!(host.calls.len(), 1);
    assert_eq!(s.tools[0].name, JsString::from("new"));
}
#[test]
fn no_context_uses_tool_pair_safe_suffix_and_no_host_calls() {
    let messages = [
        project(msg(
            "assistant",
            arr([obj([
                ("type", "tool_call".into()),
                ("id", "c".into()),
                ("name", "t".into()),
                ("input", obj([])),
            ])]),
        )),
        project(msg(
            "user",
            arr([obj([
                ("type", "tool_result".into()),
                ("toolCallId", "c".into()),
                ("content", arr([])),
            ])]),
        )),
        project(msg(
            "user",
            arr([obj([
                ("type", "tool_result_reference".into()),
                ("toolCallId", "c".into()),
                ("path", "/controlled/ref".into()),
            ])]),
        )),
        project(msg("assistant", arr([block("done")]))),
    ];
    let mut c = config();
    c.max_context_messages = Some(2.0);
    let mut host = Host::default();
    let result = assemble(&mut state(), &mut host, &c, &input(), &messages).unwrap();
    assert_eq!(
        array(result.request.value().get("messages").unwrap()).len(),
        4
    );
    assert!(host.calls.is_empty());
    c.max_context_messages = Some(0.0);
    let result = assemble(&mut state(), &mut host, &c, &input(), &messages).unwrap();
    assert!(array(result.request.value().get("messages").unwrap()).is_empty());
}
#[test]
fn orphan_prefix_null_context_source_exception_is_visible_not_silently_clamped() {
    let messages = [
        project(msg(
            "user",
            arr([obj([
                ("type", "tool_result".into()),
                ("toolCallId", "c".into()),
                ("content", arr([])),
            ])]),
        )),
        project(msg(
            "user",
            arr([obj([
                ("type", "tool_result_reference".into()),
                ("toolCallId", "c".into()),
            ])]),
        )),
    ];
    let mut c = config();
    c.max_context_messages = Some(1.0);
    let error = assemble(&mut state(), &mut Host::default(), &c, &input(), &messages).unwrap_err();
    let PortError::SourceException { message, .. } = error else {
        panic!("expected original exception")
    };
    assert_eq!(
        message,
        JsString::from("Cannot read properties of undefined (reading 'role')")
    );
}
#[test]
fn null_context_fractional_index_exception_and_nonfinite_comparisons_are_exact() {
    let messages = [
        project(msg("user", arr([block("one")]))),
        project(msg("user", arr([block("two")]))),
        project(msg("user", arr([block("three")]))),
    ];
    let mut c = config();
    c.max_context_messages = Some(2.5);
    assert!(matches!(
        assemble(&mut state(), &mut Host::default(), &c, &input(), &messages),
        Err(PortError::SourceException { .. })
    ));
    for max in [-1.0, f64::NEG_INFINITY] {
        c.max_context_messages = Some(max);
        let result = assemble(&mut state(), &mut Host::default(), &c, &input(), &messages).unwrap();
        assert!(array(result.request.value().get("messages").unwrap()).is_empty());
    }
    for max in [f64::NAN, f64::INFINITY] {
        c.max_context_messages = Some(max);
        let result = assemble(&mut state(), &mut Host::default(), &c, &input(), &messages).unwrap();
        assert_eq!(
            array(result.request.value().get("messages").unwrap()).len(),
            3
        );
    }
}
#[test]
fn prepared_prompt_truthiness_is_separate_from_nullish_config_fallback() {
    let mut c = config();
    c.system_prompt = Some("fallback".into());
    for (prompt, expected, has) in [
        (JsValue::Undefined, JsValue::from("fallback"), false),
        (JsValue::Null, JsValue::from("fallback"), false),
        (JsValue::from(""), JsValue::from(""), false),
        (b(false), b(false), false),
        (obj([]), obj([]), true),
    ] {
        let mut host = context_host(obj([
            ("messages", arr([])),
            ("tools", arr([])),
            ("systemPrompt", prompt),
        ]));
        let result = assemble(&mut state(), &mut host, &c, &input(), &[]).unwrap();
        assert_eq!(result.request.value().get("systemPrompt"), Some(&expected));
        assert_eq!(result.has_system_prompt, has);
    }
}
#[test]
fn exact_raw_prepared_messages_tools_cache_breakpoints_and_request_metadata_survive() {
    let raw = msg(
        "user",
        arr([obj([
            ("type", "unknown".into()),
            ("text", JsValue::Undefined),
            (
                "extension",
                JsValue::String(JsString::from_utf16(vec![0xd800])),
            ),
        ])]),
    );
    let mut c = config();
    c.metadata = Some(
        obj([("unknown", JsValue::Undefined)])
            .as_object()
            .unwrap()
            .clone(),
    );
    let mut host = context_host(obj([
        ("messages", arr([raw.clone()])),
        ("tools", JsValue::Null),
        ("cacheBreakpoints", JsValue::Null),
        ("preparedUnused", b(true)),
    ]));
    let result = assemble(&mut state(), &mut host, &c, &input(), &[]).unwrap();
    let request = result.request.value();
    assert_eq!(request.get("messages"), Some(&arr([raw])));
    assert_eq!(request.get("tools"), Some(&JsValue::Null));
    assert_eq!(request.get("cacheBreakpoints"), Some(&JsValue::Null));
    assert_eq!(
        request.get("metadata"),
        Some(&obj([("unknown", JsValue::Undefined)]))
    );
    assert!(request.get("preparedUnused").is_none());
    assert_eq!(request.get("cachePlan"), Some(&JsValue::Undefined));
}
#[test]
fn plan_reminder_follows_context_and_suppresses_plain_breakpoints() {
    let mut c = config();
    c.permission.mode = PermissionMode::Plan;
    let base = msg("assistant", arr([block("prepared")]));
    let mut host = context_host(obj([
        ("messages", arr([base.clone()])),
        ("tools", arr([])),
        ("cacheBreakpoints", arr([n(0.0)])),
    ]));
    let result = assemble(&mut state(), &mut host, &c, &input(), &[]).unwrap();
    let messages = array(result.request.value().get("messages").unwrap());
    assert_eq!(messages.len(), 2);
    assert_eq!(messages[0], base);
    assert_eq!(
        messages[1].get("metadata"),
        Some(&obj([
            ("synthetic", b(true)),
            ("purpose", "plan_mode_reminder".into())
        ]))
    );
    assert_eq!(
        array(messages[1].get("content").unwrap())[0].get("text"),
        Some(&JsValue::from(
            "Plan mode is active.\nRead first using read-only tools, then write or refine plan markdown only under `.pilotdeck/plans/`.\nDo not make implementation changes while planning.\nWhen the plan is ready for user review, call `exit_plan_mode` with the plan file path."
        ))
    );
    assert_eq!(
        result.request.value().get("cacheBreakpoints"),
        Some(&JsValue::Undefined)
    );
}
#[test]
fn whitespace_addenda_preserve_original_text_empty_custom_prompt_and_coordinates() {
    let mut c = config();
    c.system_prompt = Some("".into());
    c.append_system_prompt = Some("  original  ".into());
    c.plan_mode_instructions = Some("\u{feff}\u{2028}".into());
    c.permission.mode = PermissionMode::Plan;
    c.permission.additional_working_directories = vec!["".into(), "dir".into()];
    c.runtime_context_surface = RuntimeContextSurface::UserMessage;
    let mut s = state();
    s.plan_todo_addendum = Some(JsString::from_utf16(vec![0x0085, 0xd800]));
    let mut host = context_host(prepared(vec![]));
    assemble(&mut s, &mut host, &c, &input(), &[]).unwrap();
    let input = call_input(&host);
    assert_eq!(input.custom_system_prompt, Some("".into()));
    assert_eq!(
        input.append_system_prompt.as_ref().unwrap().units(),
        &[
            32, 32, 111, 114, 105, 103, 105, 110, 97, 108, 32, 32, 10, 10, 0x0085, 0xd800
        ]
    );
    assert_eq!(
        input.runtime_context_surface,
        Some(RuntimeContextSurface::UserMessage)
    );
    assert_eq!(
        input.additional_working_directories,
        vec![JsString::from(""), "dir".into()]
    );
    assert_eq!(input.permission_mode, JsString::from("plan"));
}
#[test]
fn prompt_blocks_duplicate_names_and_elicitation_requires_both_flags() {
    let mut s = state();
    let mut interactive = tool("duplicate");
    interactive.requires_user_interaction = true;
    let mut question = tool("ask_user_question");
    question.requires_user_interaction = true;
    s.tools = vec![
        interactive,
        tool("duplicate"),
        question,
        tool("enter_plan_mode"),
        tool("exit_plan_mode"),
        tool("safe"),
    ];
    let mut i = input();
    i.allow_plan_mode_tools = true;
    i.can_elicit = true;
    let mut host = context_host(prepared(vec![]));
    assemble(&mut s, &mut host, &config(), &i, &[]).unwrap();
    assert_eq!(
        call_input(&host)
            .tools
            .iter()
            .map(|t| t.name.clone())
            .collect::<Vec<_>>(),
        vec![JsString::from("exit_plan_mode"), "safe".into()]
    );
    s.elicitation_available = true;
    let mut host = context_host(prepared(vec![]));
    assemble(&mut s, &mut host, &config(), &i, &[]).unwrap();
    assert_eq!(
        call_input(&host)
            .tools
            .iter()
            .map(|t| t.name.clone())
            .collect::<Vec<_>>(),
        vec![
            JsString::from("ask_user_question"),
            "exit_plan_mode".into(),
            "safe".into()
        ]
    );
    let mut c = config();
    c.permission.can_prompt = true;
    i.allow_plan_mode_tools = false;
    let mut host = context_host(prepared(vec![]));
    assemble(&mut s, &mut host, &c, &i, &[]).unwrap();
    assert_eq!(call_input(&host).tools.len(), 4);
    assert!(
        call_input(&host)
            .tools
            .iter()
            .all(|t| !t.name.equals_str("enter_plan_mode") && !t.name.equals_str("exit_plan_mode"))
    );
}
#[test]
fn ask_tools_use_exact_allow_list_mcp_read_only_and_schema_override() {
    let mut c = config();
    c.run_mode = Some(RunMode::Ask);
    c.permission.can_prompt = true;
    let mut s = state();
    let mut mcp_read = tool("mcp_read");
    mcp_read.kind = "mcp".into();
    mcp_read.read_only = true;
    let mut mcp_write = tool("read_file");
    mcp_write.kind = "mcp".into();
    s.tools = vec![
        tool("write_file"),
        tool("agent"),
        tool("bash"),
        tool("execute_code"),
        mcp_read,
        mcp_write,
        tool("task_wait"),
    ];
    let mut host = context_host(prepared(vec![]));
    assemble(&mut s, &mut host, &c, &input(), &[]).unwrap();
    let tools = &call_input(&host).tools;
    assert_eq!(
        tools.iter().map(|t| t.name.clone()).collect::<Vec<_>>(),
        vec![
            JsString::from("agent"),
            "bash".into(),
            "execute_code".into(),
            "mcp_read".into(),
            "task_wait".into()
        ]
    );
    assert_eq!(
        tools[0].input_schema.get("required"),
        Some(&arr(["description".into(), "prompt".into()]))
    );
    assert_eq!(
        tools[0].input_schema.get("additionalProperties"),
        Some(&b(false))
    );
    let properties = tools[0].input_schema.get("properties").unwrap();
    assert_eq!(
        properties.get("subagentType").unwrap().get("description"),
        Some(&JsValue::from(
            "Deprecated legacy alias for subagent_type. Prefer subagent_type."
        ))
    );
    assert!(tools[0].description.as_ref().unwrap().to_utf8().unwrap().contains("- plan: Read-only planning subagent. Inspects code via read/grep/glob and produces a step-by-step plan. Tools: read_file, grep, glob."));
    assert_eq!(tools[1].description,Some("source description\n\n[ASK MODE] READ-ONLY commands only. Write/modify/delete commands will be rejected.".into()));
    assert_eq!(call_input(&host).run_mode, Some("ask".into()));
}
#[test]
fn metadata_uses_first_provider_protocol_exact_route_cache_and_combined_cap_priority() {
    let mut s = state();
    s.metadata = vec![
        metadata(
            route("provider", "first"),
            Some(ModelProtocol::Anthropic),
            Some(true),
            Some(9.0),
            None,
        ),
        metadata(
            route("provider", "second"),
            Some(ModelProtocol::Google),
            Some(false),
            Some(32.0),
            Some(TokenLimits {
                max_context_tokens: 1000.0,
                max_output_tokens: None,
            }),
        ),
    ];
    let mut c = config();
    c.max_output_tokens = Some(256.0);
    let mut i = input();
    i.model_override = Some(TurnModelOverride {
        route: route("provider", "second"),
        temperature: Some(0.7),
        speed: Some(-0.0),
        thinking: None,
    });
    let mut host = context_host(prepared(vec![]));
    let result = assemble(&mut s, &mut host, &c, &i, &[]).unwrap();
    let prepared = call_input(&host);
    assert_eq!(prepared.protocol, Some(ModelProtocol::Anthropic));
    assert_eq!(prepared.supports_prompt_cache, Some(false));
    assert_eq!(
        result.request.value().get("maxOutputTokens"),
        Some(&n(256.0))
    );
    assert!(result.request.value().get("temperature").is_none());
    assert_eq!(
        result
            .request
            .value()
            .get("speed")
            .unwrap()
            .as_number()
            .unwrap()
            .to_bits(),
        (-0.0f64).to_bits()
    );
    assert_eq!(result.route, route("provider", "second"));
}
#[test]
fn recovery_caps_follow_attempt_requested_session_baseline_hard_and_model_min_floor() {
    let mut s = state();
    let mut tokens = cap();
    tokens.attempt_max_output_tokens = Some(100.9);
    tokens.requested_max_output_tokens = Some(80.9);
    tokens.session_max_output_tokens = Some(60.9);
    tokens.hard_max_output_tokens = Some(90.9);
    s.metadata = vec![metadata(
        route("provider", "model"),
        None,
        None,
        Some(95.9),
        None,
    )];
    let mut c = config();
    c.max_output_tokens = Some(40.9);
    c.is_subagent = Some(true);
    c.subagent_model = Some(TurnSubagentModel {
        route: route("provider", "model"),
        max_context_tokens: None,
        max_output_tokens: Some(50.9),
    });
    for (attempt, requested, session, expected) in [
        (Some(100.9), Some(80.9), Some(60.9), 90.0),
        (None, Some(80.9), Some(60.9), 80.0),
        (None, None, Some(60.9), 60.0),
        (None, None, None, 50.0),
    ] {
        tokens.attempt_max_output_tokens = attempt;
        tokens.requested_max_output_tokens = requested;
        tokens.session_max_output_tokens = session;
        s.token_caps = vec![tokens.clone()];
        let result = assemble(&mut s, &mut Host::default(), &c, &input(), &[]).unwrap();
        assert_eq!(
            result.request.value().get("maxOutputTokens"),
            Some(&n(expected))
        );
    }
    tokens.attempt_max_output_tokens = Some(f64::NAN);
    tokens.hard_max_output_tokens = None;
    s.token_caps = vec![tokens];
    let result = assemble(&mut s, &mut Host::default(), &c, &input(), &[]).unwrap();
    assert_eq!(
        result.request.value().get("maxOutputTokens"),
        Some(&n(40.9)),
        "nonnull invalid attempt suppresses lower requested caps; final config fallback stays exact"
    );
}
#[test]
fn thinking_override_and_stream_fields_are_exact_and_immutable() {
    let mut c = config();
    c.thinking = Some(ThinkingConfig {
        enabled: true,
        mode: Some(ThinkingMode::High),
        budget_tokens: None,
        preserve: None,
        split_reasoning: None,
    });
    c.tool_choice = Some(ToolChoice::Tool {
        name: JsString::from_utf16(vec![0xd800]),
    });
    let mut i = input();
    i.model_override = Some(TurnModelOverride {
        route: route("p", "m"),
        temperature: Some(1.0),
        speed: None,
        thinking: Some(ThinkingConfig {
            enabled: false,
            mode: Some(ThinkingMode::Off),
            budget_tokens: Some(2.0),
            preserve: Some(false),
            split_reasoning: Some(true),
        }),
    });
    let result = assemble(&mut state(), &mut Host::default(), &c, &i, &[]).unwrap();
    let request = result.request.value();
    assert_eq!(
        request.get("thinking"),
        Some(&obj([
            ("enabled", b(false)),
            ("mode", "off".into()),
            ("budgetTokens", n(2.0)),
            ("preserve", b(false)),
            ("splitReasoning", b(true))
        ]))
    );
    assert_eq!(
        request
            .get("toolChoice")
            .unwrap()
            .get("name")
            .unwrap()
            .as_string()
            .unwrap()
            .units(),
        &[0xd800]
    );
    assert_eq!(request.get("stream"), Some(&b(true)));
    assert_eq!(request.get("speed"), Some(&JsValue::Undefined));
    assert!(request.get("temperature").is_none());
    c.thinking = None;
    assert_eq!(
        request.get("thinking").unwrap().get("enabled"),
        Some(&b(false))
    );
}
#[test]
fn media_profiles_fail_visibly_but_inline_nested_refs_and_cache_are_preserved() {
    let media = obj([
        ("type", "media_reference".into()),
        ("path", "/controlled/unread".into()),
        ("mediaType", "image".into()),
    ]);
    let mut host = context_host(prepared(vec![msg("user", arr([media.clone()]))]));
    let error = assemble(&mut state(), &mut host, &config(), &input(), &[]).unwrap_err();
    let PortError::HostFailure { code, .. } = error else {
        panic!("expected explicit profile gap")
    };
    assert_eq!(
        code,
        JsString::from("REQUEST_MEDIA_REFERENCE_MATERIALIZATION_PROFILE")
    );
    let nested = obj([
        ("type", "tool_result".into()),
        ("toolCallId", "c".into()),
        ("content", arr([media])),
    ]);
    let inline = obj([
        ("type", "image".into()),
        ("source", "base64".into()),
        ("data", "fixture".into()),
        ("mimeType", "image/png".into()),
    ]);
    let mut host = context_host(prepared(vec![msg(
        "user",
        arr([nested.clone(), inline.clone()]),
    )]));
    let result = assemble(&mut state(), &mut host, &config(), &input(), &[]).unwrap();
    assert_eq!(
        result.request.value().get("messages"),
        Some(&arr([msg("user", arr([nested, inline]))]))
    );
    let mut host = context_host(obj([
        ("messages", arr([])),
        ("tools", arr([])),
        ("cachePlan", obj([("generation", n(1.0))])),
    ]));
    let result = assemble(&mut state(), &mut host, &config(), &input(), &[]).unwrap();
    assert_eq!(
        result
            .request
            .value()
            .get("cachePlan")
            .unwrap()
            .get("generation"),
        Some(&n(1.0))
    );
    assert_eq!(
        result.request.value().get("cacheBreakpoints"),
        Some(&arr([]))
    );
    for cache in [
        JsValue::Null,
        JsValue::Undefined,
        b(false),
        n(0.0),
        "".into(),
    ] {
        let mut host = context_host(obj([
            ("messages", arr([])),
            ("tools", arr([])),
            ("cachePlan", cache),
            ("cacheBreakpoints", JsValue::Undefined),
        ]));
        assert_eq!(
            assemble(&mut state(), &mut host, &config(), &input(), &[])
                .unwrap()
                .request
                .value()
                .get("cacheBreakpoints"),
            Some(&JsValue::Undefined)
        );
    }
}
#[test]
fn shallow_prepared_null_missing_messages_and_wrong_reply_are_not_fabricated() {
    for value in [
        JsValue::Null,
        JsValue::Undefined,
        obj([]),
        obj([("messages", JsValue::Null)]),
        obj([("messages", "invalid".into())]),
    ] {
        let mut host = context_host(value);
        assert!(matches!(
            assemble(&mut state(), &mut host, &config(), &input(), &[]),
            Err(PortError::SourceException { .. })
        ));
    }
    let mut host = Host {
        context: true,
        replies: VecDeque::from([Ok(HostReply::EventEmitted)]),
        ..Host::default()
    };
    assert!(matches!(
        assemble(&mut state(), &mut host, &config(), &input(), &[]),
        Err(PortError::InvalidResponse { .. })
    ));
    assert_eq!(host.calls.len(), 1);
}

#[test]
fn instructions_notice_precedes_finalizer_failures_and_propagates_observer_error() {
    let cases = [
        obj([
            ("systemPrompt", "loaded".into()),
            ("messages", JsValue::Null),
        ]),
        obj([
            ("systemPrompt", "loaded".into()),
            (
                "messages",
                arr([msg(
                    "user",
                    arr([obj([
                        ("type", "media_reference".into()),
                        ("path", "unread".into()),
                    ])]),
                )]),
            ),
        ]),
        obj([
            ("systemPrompt", "loaded".into()),
            ("messages", arr([])),
            ("cachePlan", obj([])),
        ]),
    ];
    for prepared in cases {
        let mut host = context_host(prepared);
        let mut observer = Observer::default();
        assert!(
            request_assembly::assemble(
                &mut state(),
                &mut host,
                &control(),
                &config(),
                &input(),
                &[],
                &mut observer
            )
            .is_err()
        );
        assert_eq!(observer.notices, vec![true]);
        assert_eq!(host.calls.len(), 1);
    }
    let mut host = context_host(obj([
        ("systemPrompt", "".into()),
        ("messages", JsValue::Null),
    ]));
    let mut observer = Observer {
        fail: true,
        ..Observer::default()
    };
    assert!(
        matches!(request_assembly::assemble(&mut state(),&mut host,&control(),&config(),&input(),&[],&mut observer),Err(PortError::TransportBeforeDispatch{message})if message.equals_str("observer failure"))
    );
    assert_eq!(observer.notices, vec![false]);
    let mut observer = Observer::default();
    let mut host = context_host(JsValue::Null);
    assert!(
        request_assembly::assemble(
            &mut state(),
            &mut host,
            &control(),
            &config(),
            &input(),
            &[],
            &mut observer
        )
        .is_err()
    );
    assert!(observer.notices.is_empty());
}

#[test]
fn cancellation_during_context_ack_keeps_source_notice_before_abort_barrier() {
    let control = control();
    let mut host = context_host(obj([
        ("systemPrompt", "acknowledged".into()),
        ("messages", JsValue::Null),
    ]));
    host.cancel_after = Some((1, control.cancellation.clone()));
    let mut observer = Observer::default();
    assert!(matches!(
        request_assembly::assemble(
            &mut state(),
            &mut host,
            &control,
            &config(),
            &input(),
            &[],
            &mut observer
        ),
        Err(PortError::Cancelled { .. })
    ));
    assert_eq!(observer.notices, vec![true]);
    assert_eq!(host.calls.len(), 1);
}

fn options_assemble(
    s: &mut RequestPreparationState,
    h: &mut Host,
    c: &TurnRuntimeConfig,
    i: &TurnInput,
    options: RequestAssemblyOptions,
    observer: &mut Observer,
) -> PortResult<request::PreparedRequestIntent> {
    request_assembly::assemble_with_options(
        s,
        h,
        RequestAssemblyInput {
            control: &control(),
            config: c,
            input: i,
            messages: &[],
        },
        options,
        observer,
    )
}
#[test]
fn default_shim_retains_undefined_preview_and_instruction_notice() {
    let mut h = context_host(obj([
        ("messages", arr([])),
        ("systemPrompt", "loaded".into()),
    ]));
    let mut observer = Observer::default();
    let result = request_assembly::assemble(
        &mut state(),
        &mut h,
        &control(),
        &config(),
        &input(),
        &[],
        &mut observer,
    )
    .unwrap();
    assert_eq!(call_input(&h).preview_only, None);
    assert_eq!(observer.notices, vec![true]);
    assert!(result.has_system_prompt);
    assert_eq!(
        RequestAssemblyOptions::default(),
        RequestAssemblyOptions {
            preview_only: None,
            emit_instruction_events: true
        }
    );
}
#[test]
fn preview_none_false_true_and_notification_flag_are_independent() {
    for preview_only in [None, Some(false), Some(true)] {
        for emit_instruction_events in [false, true] {
            let mut h = context_host(obj([
                ("messages", arr([])),
                ("systemPrompt", "loaded".into()),
            ]));
            let mut observer = Observer::default();
            let result = options_assemble(
                &mut state(),
                &mut h,
                &config(),
                &input(),
                RequestAssemblyOptions {
                    preview_only,
                    emit_instruction_events,
                },
                &mut observer,
            )
            .unwrap();
            assert_eq!(call_input(&h).preview_only, preview_only);
            assert_eq!(
                observer.notices,
                if emit_instruction_events {
                    vec![true]
                } else {
                    vec![]
                }
            );
            assert!(result.has_system_prompt);
            assert_eq!(h.calls.len(), 1);
        }
    }
}
#[test]
fn preview_suppression_skips_observer_and_retains_null_prepared_error_precedence() {
    let options = RequestAssemblyOptions {
        preview_only: Some(true),
        emit_instruction_events: false,
    };
    let mut h = context_host(JsValue::Null);
    let mut observer = Observer {
        fail: true,
        ..Observer::default()
    };
    let error = options_assemble(
        &mut state(),
        &mut h,
        &config(),
        &input(),
        options,
        &mut observer,
    )
    .unwrap_err();
    assert!(
        matches!(error,PortError::SourceException{message,..}if message.equals_str("Cannot read properties of null (reading 'messages')"))
    );
    assert!(observer.notices.is_empty());
    assert_eq!(call_input(&h).preview_only, Some(true));
    let mut h = context_host(JsValue::Null);
    let mut observer = Observer::default();
    let error = options_assemble(
        &mut state(),
        &mut h,
        &config(),
        &input(),
        RequestAssemblyOptions::default(),
        &mut observer,
    )
    .unwrap_err();
    assert!(
        matches!(error,PortError::SourceException{message,..}if message.equals_str("Cannot read properties of null (reading 'systemPrompt')"))
    );
    assert!(observer.notices.is_empty());
}
#[test]
fn suppressed_notice_cannot_replace_finalizer_error_but_default_notice_error_can() {
    for emit_instruction_events in [false, true] {
        let mut h = context_host(obj([
            ("messages", JsValue::Null),
            ("systemPrompt", "loaded".into()),
        ]));
        let mut observer = Observer {
            fail: true,
            ..Observer::default()
        };
        let error = options_assemble(
            &mut state(),
            &mut h,
            &config(),
            &input(),
            RequestAssemblyOptions {
                preview_only: Some(true),
                emit_instruction_events,
            },
            &mut observer,
        )
        .unwrap_err();
        if emit_instruction_events {
            assert!(
                matches!(error,PortError::TransportBeforeDispatch{message}if message.equals_str("observer failure"))
            );
            assert_eq!(observer.notices, vec![true]);
        } else {
            assert!(
                matches!(error,PortError::SourceException{message,..}if message.equals_str("Cannot read properties of null (reading 'map')"))
            );
            assert!(observer.notices.is_empty());
        }
    }
}
#[test]
fn suppressed_media_cache_and_context_ack_cancellation_make_no_notice() {
    for value in [
        obj([
            (
                "messages",
                arr([msg(
                    "user",
                    arr([obj([
                        ("type", "media_reference".into()),
                        ("path", "unread".into()),
                    ])]),
                )]),
            ),
            ("systemPrompt", "loaded".into()),
        ]),
        obj([
            ("messages", arr([])),
            ("cachePlan", obj([])),
            ("systemPrompt", "loaded".into()),
        ]),
    ] {
        let mut h = context_host(value);
        let mut observer = Observer::default();
        assert!(matches!(
            options_assemble(
                &mut state(),
                &mut h,
                &config(),
                &input(),
                RequestAssemblyOptions {
                    preview_only: None,
                    emit_instruction_events: false
                },
                &mut observer
            ),
            Err(PortError::HostFailure { .. } | PortError::SourceException { .. })
        ));
        assert!(observer.notices.is_empty());
    }
    let ctl = control();
    let mut h = context_host(prepared(vec![]));
    h.cancel_after = Some((1, ctl.cancellation.clone()));
    let mut observer = Observer::default();
    assert!(matches!(
        request_assembly::assemble_with_options(
            &mut state(),
            &mut h,
            RequestAssemblyInput {
                control: &ctl,
                config: &config(),
                input: &input(),
                messages: &[]
            },
            RequestAssemblyOptions {
                preview_only: Some(true),
                emit_instruction_events: false
            },
            &mut observer
        ),
        Err(PortError::Cancelled { .. })
    ));
    assert!(observer.notices.is_empty());
    assert_eq!(h.calls.len(), 1);
}
#[test]
fn joined_nul_metadata_last_replacement_keeps_first_slot_for_provider_protocol() {
    let a = route("p", "\0m");
    let b = route("p\0", "m");
    let mut s = state();
    s.metadata = vec![
        metadata(
            a.clone(),
            Some(ModelProtocol::Anthropic),
            Some(true),
            Some(8.0),
            None,
        ),
        metadata(
            route("p", "other"),
            Some(ModelProtocol::OpenAi),
            Some(true),
            Some(64.0),
            None,
        ),
        metadata(
            b.clone(),
            Some(ModelProtocol::Google),
            Some(false),
            Some(16.0),
            None,
        ),
    ];
    let mut c = config();
    c.route = a.clone();
    c.max_output_tokens = Some(100.0);
    let mut h = context_host(prepared(vec![]));
    let result = assemble(&mut s, &mut h, &c, &input(), &[]).unwrap();
    assert_eq!(call_input(&h).protocol, Some(ModelProtocol::OpenAi));
    assert_eq!(call_input(&h).supports_prompt_cache, Some(false));
    assert_eq!(
        result.request.value().get("maxOutputTokens"),
        Some(&n(16.0))
    );
    assert_eq!(
        s.metadata.len(),
        3,
        "read projection must not mutate owner snapshots"
    );
    c.route = b;
    let mut h = context_host(prepared(vec![]));
    assemble(&mut s, &mut h, &c, &input(), &[]).unwrap();
    assert_eq!(call_input(&h).protocol, Some(ModelProtocol::Google));
    // Same-provider replacement also stays in the first key's insertion slot.
    s.metadata = vec![
        metadata(
            a.clone(),
            Some(ModelProtocol::Anthropic),
            Some(true),
            None,
            None,
        ),
        metadata(
            route("p", "other"),
            Some(ModelProtocol::OpenAi),
            None,
            None,
            None,
        ),
        metadata(a.clone(), Some(ModelProtocol::Google), None, None, None),
    ];
    c.route = a;
    let mut h = context_host(prepared(vec![]));
    assemble(&mut s, &mut h, &c, &input(), &[]).unwrap();
    assert_eq!(call_input(&h).protocol, Some(ModelProtocol::Google));
}
#[test]
fn joined_nul_caps_use_latest_whole_entry_but_subagent_baseline_stays_exact_pair() {
    let a = route("p", "\0m");
    let b = route("p\0", "m");
    let mut first = cap();
    first.route = a.clone();
    first.session_max_output_tokens = Some(40.0);
    let mut last = first.clone();
    last.route = b.clone();
    last.session_max_output_tokens = Some(20.0);
    let mut s = state();
    s.token_caps = vec![first, last.clone()];
    let mut c = config();
    c.route = a.clone();
    c.max_output_tokens = Some(100.0);
    let result = assemble(&mut s, &mut Host::default(), &c, &input(), &[]).unwrap();
    assert_eq!(
        result.request.value().get("maxOutputTokens"),
        Some(&n(20.0))
    );
    last.session_max_output_tokens = None;
    s.token_caps.push(last);
    let result = assemble(&mut s, &mut Host::default(), &c, &input(), &[]).unwrap();
    assert_eq!(
        result.request.value().get("maxOutputTokens"),
        Some(&n(100.0)),
        "latest Map.set replaces whole cap, not fieldwise merge"
    );
    s.token_caps.clear();
    c.is_subagent = Some(true);
    c.subagent_model = Some(TurnSubagentModel {
        route: b.clone(),
        max_context_tokens: None,
        max_output_tokens: Some(7.0),
    });
    let result = assemble(&mut s, &mut Host::default(), &c, &input(), &[]).unwrap();
    assert_eq!(
        result.request.value().get("maxOutputTokens"),
        Some(&n(100.0)),
        "same joined key cannot match exact baseline route"
    );
    c.route = b;
    let result = assemble(&mut s, &mut Host::default(), &c, &input(), &[]).unwrap();
    assert_eq!(result.request.value().get("maxOutputTokens"), Some(&n(7.0)));
}
#[test]
fn joined_nul_keys_keep_utf16_surrogates_and_do_not_conflate_replacement_characters() {
    let a = Route {
        provider: JsString::from_utf16(vec![0xd800]),
        model: JsString::from_utf16(vec![0, 109]),
    };
    let b = Route {
        provider: JsString::from_utf16(vec![0xd800, 0]),
        model: "m".into(),
    };
    let replacement = Route {
        provider: JsString::from_utf16(vec![0xfffd]),
        model: JsString::from_utf16(vec![0, 109]),
    };
    let mut s = state();
    s.metadata = vec![
        metadata(
            b,
            Some(ModelProtocol::Google),
            Some(false),
            Some(17.0),
            None,
        ),
        metadata(
            replacement,
            Some(ModelProtocol::Anthropic),
            Some(true),
            Some(5.0),
            None,
        ),
    ];
    let mut c = config();
    c.route = a;
    c.max_output_tokens = Some(100.0);
    let mut h = context_host(prepared(vec![]));
    let result = assemble(&mut s, &mut h, &c, &input(), &[]).unwrap();
    assert_eq!(
        result.request.value().get("maxOutputTokens"),
        Some(&n(17.0))
    );
    assert_eq!(call_input(&h).supports_prompt_cache, Some(false));
    assert_eq!(
        call_input(&h).protocol,
        None,
        "provider-only protocol must not use joined key or lossy UTF8"
    );
}

fn cached_request(
    cache: JsValue,
    messages: Vec<JsValue>,
    tools: JsValue,
) -> request::PreparedRequestIntent {
    let mut c = config();
    c.route = route("p", "m");
    c.system_prompt = Some("fallback".into());
    let mut h = context_host(obj([
        ("messages", JsValue::Array(messages)),
        ("tools", tools),
        ("cachePlan", cache),
    ]));
    assemble(&mut state(), &mut h, &c, &input(), &[]).unwrap()
}

#[test]
fn six_actual_original_cache_plan_profiles_match_frozen_hash_and_own_generation() {
    // Guarded original createModelRequest observations 065,067,069,070,071,072
    // in CORE01-request-invocation-review-a3; these are actual Node results,
    // not a hash calculated by this author helper as its own expected value.
    for cache in [
        b(true),
        n(1.0),
        JsValue::String(JsString::from_utf16(
            "prepared".encode_utf16().chain([0xd800, 0x4e2d]).collect(),
        )),
        arr([]),
        obj([]),
        obj([("unknown", JsValue::Undefined)]),
    ] {
        let result = cached_request(cache, vec![msg("user", arr([block("hello")]))], arr([]));
        let r = result.request.value();
        assert_eq!(r.get("systemPrompt"), Some(&JsValue::from("fallback")));
        assert_eq!(r.get("cacheBreakpoints"), Some(&arr([n(0.0)])));
        let plan = r.get("cachePlan").unwrap();
        assert_eq!(plan.get("provider"), Some(&JsValue::from("p")));
        assert_eq!(plan.get("model"), Some(&JsValue::from("m")));
        assert_eq!(plan.get("system"), Some(&b(false)));
        assert_eq!(plan.get("tools"), Some(&b(false)));
        assert_eq!(plan.get("messages"), Some(&arr([n(0.0)])));
        assert_eq!(plan.get("generation"), Some(&JsValue::Undefined));
        assert_eq!(
            plan.get("fingerprint"),
            Some(&JsValue::from(
                "e4dce483654c614b9e99db500531702d72db625f5bf874f7ed5dfd0de5fc8b59"
            ))
        );
    }
}

#[test]
fn stable_serialization_is_distinct_from_ordered_json_replay_identity() {
    let left = obj([("b", n(2.0)), ("a", n(1.0))]);
    let right = obj([("a", n(1.0)), ("b", n(2.0))]);
    assert_eq!(
        request_assembly::stable_serialize(&left).unwrap(),
        Some("{\"a\":1,\"b\":2}".into())
    );
    assert_eq!(
        request_assembly::stable_serialize(&left).unwrap(),
        request_assembly::stable_serialize(&right).unwrap()
    );
    assert_ne!(
        pilotdeck_protocol::stringify_json(&left).unwrap(),
        pilotdeck_protocol::stringify_json(&right).unwrap()
    );
    assert_eq!(
        request_assembly::stable_serialize(&obj([("10", n(10.0)), ("2", n(2.0))])).unwrap(),
        Some("{\"10\":10,\"2\":2}".into())
    );
}

#[test]
fn stable_serialization_preserves_undefined_join_slots_and_nonfinite_json_numbers() {
    assert_eq!(
        request_assembly::stable_serialize(&JsValue::Undefined).unwrap(),
        None
    );
    let value = obj([
        ("u", JsValue::Undefined),
        (
            "a",
            arr([JsValue::Undefined, JsValue::Null, JsValue::Undefined]),
        ),
        ("n", arr([n(f64::NAN), n(f64::INFINITY), n(-0.0)])),
    ]);
    assert_eq!(
        request_assembly::stable_serialize(&value).unwrap(),
        Some("{\"a\":[,null,],\"n\":[null,null,0],\"u\":undefined}".into())
    );
    assert_eq!(
        request_assembly::stable_serialize(&arr([JsValue::Undefined])).unwrap(),
        Some("[]".into())
    );
    assert_eq!(
        request_assembly::stable_serialize(&arr([JsValue::Undefined, JsValue::Undefined])).unwrap(),
        Some("[,]".into())
    );
}

#[test]
fn stable_serialization_sorts_utf16_keys_and_hashes_escaped_lone_surrogates() {
    let mut object = JsObject::new();
    object.insert(JsString::from_utf16(vec![0xe000]), n(2.0));
    object.insert(JsString::from_utf16(vec![0xd800, 0xdc00]), n(1.0));
    object.insert(
        "x".into(),
        JsValue::String(JsString::from_utf16(vec![0xd800])),
    );
    assert_eq!(
        request_assembly::stable_serialize(&JsValue::Object(object)).unwrap(),
        Some("{\"x\":\"\\ud800\",\"𐀀\":1,\"\u{e000}\":2}".into())
    );
    let lone_block = obj([
        ("type", "text".into()),
        ("text", JsValue::String(JsString::from_utf16(vec![0xd800]))),
    ]);
    let lone = cached_request(b(true), vec![msg("user", arr([lone_block]))], arr([]));
    let replacement = cached_request(b(true), vec![msg("user", arr([block("�")]))], arr([]));
    assert_ne!(
        lone.request
            .value()
            .get("cachePlan")
            .unwrap()
            .get("fingerprint"),
        replacement
            .request
            .value()
            .get("cachePlan")
            .unwrap()
            .get("fingerprint")
    );
}

#[test]
fn generation_remains_lossless_and_never_contributes_to_cache_fingerprint() {
    let first = cached_request(obj([]), vec![], arr([]));
    for generation in [
        JsValue::Undefined,
        JsValue::Null,
        b(false),
        n(-0.0),
        n(f64::NAN),
        "not-a-number".into(),
        arr([JsValue::Undefined]),
        obj([("unknown", JsValue::Undefined)]),
    ] {
        let result = cached_request(obj([("generation", generation.clone())]), vec![], arr([]));
        let actual = result
            .request
            .value()
            .get("cachePlan")
            .unwrap()
            .get("generation")
            .unwrap();
        if let Some(number) = generation.as_number() {
            assert_eq!(actual.as_number().unwrap().to_bits(), number.to_bits());
        } else {
            assert_eq!(actual, &generation);
        }
        assert_eq!(
            result
                .request
                .value()
                .get("cachePlan")
                .unwrap()
                .get("fingerprint"),
            first
                .request
                .value()
                .get("cachePlan")
                .unwrap()
                .get("fingerprint")
        );
    }
}

#[test]
fn cache_recent_breakpoints_keep_final_three_non_system_original_indices() {
    let messages = vec![
        msg("user", arr([])),
        msg("system", arr([])),
        msg("assistant", arr([block("a")])),
        msg("user", arr([])),
        msg("system", arr([])),
        obj([("content", arr([]))]),
    ];
    let result = cached_request(b(true), messages, arr([]));
    assert_eq!(
        result.request.value().get("cacheBreakpoints"),
        Some(&arr([n(2.0), n(3.0), n(5.0)]))
    );
}

#[test]
fn cache_rebuild_includes_plan_reminder_after_inline_media_finalization() {
    let mut c = config();
    c.permission.mode = PermissionMode::Plan;
    let messages = vec![
        msg(
            "user",
            arr([obj([
                ("type", "image".into()),
                ("source", "base64".into()),
                ("data", "controlled".into()),
            ])]),
        ),
        msg("system", arr([])),
        msg("user", arr([block("last")])),
    ];
    let prepared = obj([
        ("messages", arr(messages.clone())),
        ("tools", arr([])),
        ("cachePlan", obj([("generation", n(3.0))])),
        ("cacheBreakpoints", arr([n(99.0)])),
    ]);
    let mut h = context_host(prepared.clone());
    let plan = assemble(&mut state(), &mut h, &c, &input(), &[]).unwrap();
    assert_eq!(
        plan.request.value().get("cacheBreakpoints"),
        Some(&arr([n(0.0), n(2.0), n(3.0)]))
    );
    let output = array(plan.request.value().get("messages").unwrap());
    assert_eq!(output.len(), 4);
    assert_eq!(&output[..3], messages.as_slice());
    c.permission.mode = PermissionMode::Default;
    let mut h = context_host(prepared);
    let normal = assemble(&mut state(), &mut h, &c, &input(), &[]).unwrap();
    assert_ne!(
        plan.request
            .value()
            .get("cachePlan")
            .unwrap()
            .get("fingerprint"),
        normal
            .request
            .value()
            .get("cachePlan")
            .unwrap()
            .get("fingerprint")
    );
}

#[test]
fn cache_zero_one_tool_profiles_do_not_invent_name_validation_or_mutate_tools() {
    for tools in [
        arr([]),
        arr([JsValue::Null]),
        arr([JsValue::Undefined]),
        arr([n(1.0)]),
        arr([obj([("name", JsValue::Undefined)])]),
        "".into(),
        "x".into(),
        JsValue::String(JsString::from_utf16(vec![0xd800, 0xdc00])),
    ] {
        let result = cached_request(b(true), vec![], tools.clone());
        assert_eq!(result.request.value().get("tools"), Some(&tools));
        assert_eq!(
            result
                .request
                .value()
                .get("cachePlan")
                .unwrap()
                .get("tools"),
            Some(&b(false))
        );
    }
}

#[test]
fn cache_invalid_iterable_and_multi_tool_collation_have_distinct_failures_after_notice() {
    for tools in [JsValue::Null, JsValue::Undefined, n(1.0), b(true), obj([])] {
        let mut h = context_host(obj([
            ("messages", arr([])),
            ("tools", tools),
            ("cachePlan", b(true)),
        ]));
        let mut observer = Observer::default();
        let error = request_assembly::assemble(
            &mut state(),
            &mut h,
            &control(),
            &config(),
            &input(),
            &[],
            &mut observer,
        )
        .unwrap_err();
        assert!(
            matches!(error,PortError::SourceException{message,..}if message.equals_str("input.tools is not iterable"))
        );
        assert_eq!(observer.notices, vec![false]);
    }
    let mut h = context_host(obj([
        ("messages", arr([])),
        (
            "tools",
            arr([obj([("name", "z".into())]), obj([("name", "a".into())])]),
        ),
        ("cachePlan", b(true)),
    ]));
    assert!(
        matches!(assemble(&mut state(),&mut h,&config(),&input(),&[]),Err(PortError::HostFailure{code,..})if code.equals_str("REQUEST_CACHE_LOCALE_COLLATION_PROFILE"))
    );
}

#[test]
fn cache_native_recursive_depth_profile_is_explicit_unfinished_scope() {
    let mut value = JsValue::Null;
    for _ in 0..1024 {
        value = arr([value]);
    }
    assert!(
        matches!(request_assembly::stable_serialize(&value),Err(PortError::HostFailure{code,..})if code.equals_str("REQUEST_CACHE_NATIVE_RECURSION_PROFILE"))
    );
}

#[test]
fn compaction_combined_limits_override_output_and_supply_context_fallback() {
    // Independent source review B01: AgentLoop2559–2597, not raw request fields.
    let mut s = state();
    let mut c = config();
    c.max_output_tokens = Some(1000.0);
    for (combined_output, expected) in [(Some(500.0), 500.0), (None, 1000.0)] {
        s.metadata = vec![metadata(
            c.route.clone(),
            None,
            None,
            Some(900.0),
            Some(TokenLimits {
                max_context_tokens: 128000.0,
                max_output_tokens: combined_output,
            }),
        )];
        assert_eq!(
            request_assembly::compaction_limits(&s, &c, &c.route, true),
            (128000.0, expected)
        );
    }
}

#[test]
fn compaction_routes_keep_source_joined_key_collisions_and_latest_snapshot() {
    // Independent source review B02: source Map key is joined UTF16, not a pair.
    let requested = route("p\0q", "r");
    let collision = route("p", "q\0r");
    let mut s = state();
    let mut c = config();
    c.max_output_tokens = Some(1000.0);
    s.token_caps = vec![RouteTokenCaps {
        route: collision.clone(),
        max_context_tokens: Some(123000.0),
        attempt_max_output_tokens: None,
        requested_max_output_tokens: None,
        session_max_output_tokens: Some(800.0),
        hard_max_output_tokens: Some(600.0),
    }];
    s.metadata = vec![
        metadata(requested.clone(), None, None, Some(900.0), None),
        metadata(
            collision,
            None,
            None,
            None,
            Some(TokenLimits {
                max_context_tokens: 128000.0,
                max_output_tokens: Some(512.0),
            }),
        ),
    ];
    assert_eq!(
        request_assembly::compaction_limits(&s, &c, &requested, false),
        (123000.0, 512.0)
    );
}

#[test]
fn compaction_subagent_pre_route_ignores_parent_config_but_reserved_caps_follow_route() {
    let mut c = config();
    c.max_context_tokens = Some(64000.0);
    c.max_output_tokens = Some(1000.0);
    c.is_subagent = Some(true);
    let child = route("child", "model");
    c.subagent_model = Some(TurnSubagentModel {
        route: child.clone(),
        max_context_tokens: Some(128000.0),
        max_output_tokens: Some(512.0),
    });
    let mut s = state();
    s.metadata = vec![metadata(child.clone(), None, None, Some(400.0), None)];
    assert_eq!(
        request_assembly::compaction_limits(&s, &c, &c.route, true),
        (1000000.0, 0.0)
    );
    assert_eq!(
        request_assembly::compaction_limits(&s, &c, &child, false),
        (128000.0, 400.0)
    );
}
