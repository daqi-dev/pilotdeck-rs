use pilotdeck_agent::ports::*;
use pilotdeck_agent::{MessageProjection, ToolContent};
use pilotdeck_protocol::admission::CallEffect;
use pilotdeck_protocol::model::{BoundarySnapshot, ContentBody, ModelEvent, Usage};
use pilotdeck_protocol::{JsObject, JsString, JsValue, parse_json};
use pilotdeck_transport::*;

fn json(s: &str) -> JsValue {
    parse_json(s).unwrap()
}
fn object(pairs: &[(&str, JsValue)]) -> JsObject {
    let mut o = JsObject::new();
    for (k, v) in pairs {
        o.insert((*k).into(), v.clone());
    }
    o
}
fn owner() -> ExecutionIdentity {
    ExecutionIdentity {
        session_id: "s".into(),
        turn_id: "t".into(),
        run_id: "r".into(),
        operation_id: "o".into(),
        request_id: "actual-request".into(),
        idempotency_key: Some("key".into()),
        module_instance_id: "instance".into(),
        connection_generation: "generation".into(),
    }
}
fn response(payload: JsValue) -> JsValue {
    JsValue::Object(object(&[
        ("kind", "response".into()),
        ("messageId", "m".into()),
        ("inReplyTo", "reply".into()),
        ("ok", JsValue::Bool(true)),
        ("payload", payload),
    ]))
}
#[derive(Debug)]
struct Exchange {
    responses: Vec<JsValue>,
    calls: Vec<(&'static str, &'static str, CallEffect, JsObject)>,
}
impl HostExchange for Exchange {
    fn exchange(
        &mut self,
        _: &CallControl,
        m: &'static str,
        op: &'static str,
        e: CallEffect,
        p: JsObject,
    ) -> PortResult<HostExchangeResponse> {
        self.calls.push((m, op, e, p));
        Ok(HostExchangeResponse {
            identity: owner(),
            cancelled_before_response: false,
            boundary: BoundarySnapshot::capture(&self.responses.remove(0)),
        })
    }
}
fn host(values: Vec<JsValue>) -> RemoteHost<Exchange> {
    let modules = [
        (
            "model",
            vec![
                "get_metadata",
                "prepare",
                "materialize_prepared_request",
                "stream_next",
                "close_stream",
                "stream",
            ],
        ),
        (
            "budget",
            vec![
                "estimate_request_input",
                "evaluate_request_budget",
                "estimate_usage_cost",
            ],
        ),
        (
            "turn",
            vec!["drain_steer", "drain_or_close_steer", "persist_compaction"],
        ),
        (
            "capability",
            vec!["list_tools", "execute_batch", "execute", "plan_todo"],
        ),
        ("permission", vec!["decide"]),
        (
            "context",
            vec![
                "prepare_for_model",
                "apply_tool_results",
                "recover_from_model_error",
                "capture_turn",
                "try_auto_compact",
            ],
        ),
        ("lifecycle", vec!["dispatch"]),
        ("event", vec!["emit"]),
    ]
    .into_iter()
    .map(|(m, ops)| (m.into(), ops.into_iter().map(Into::into).collect()))
    .collect();
    RemoteHost::new(
        Exchange {
            responses: values,
            calls: vec![],
        },
        AdvertisedMethods { modules },
    )
}
struct ClockExchange {
    inner: Exchange,
    samples: std::cell::Cell<usize>,
}
impl HostExchange for ClockExchange {
    fn wall_now_iso(&self) -> JsString {
        let sample = self.samples.get();
        self.samples.set(sample + 1);
        format!("2026-10-09T00:00:00.{sample:03}Z").into()
    }
    fn exchange(
        &mut self,
        control: &CallControl,
        module: &'static str,
        operation: &'static str,
        effect: CallEffect,
        payload: JsObject,
    ) -> PortResult<HostExchangeResponse> {
        self.inner
            .exchange(control, module, operation, effect, payload)
    }
}
fn clock_host(value: JsValue) -> RemoteHost<ClockExchange> {
    RemoteHost::new(
        ClockExchange {
            inner: host(vec![value]).into_exchange(),
            samples: std::cell::Cell::new(0),
        },
        AdvertisedMethods {
            modules: vec![("capability".into(), vec!["execute_batch".into()])],
        },
    )
}

#[test]
fn known_failed_batch_returns_module_results_with_independent_clocks_and_raw_details() {
    let raw = json(
        r#"{"kind":"response","messageId":"m","inReplyTo":"reply","ok":false,"code":"future_code","error":{"message":["x",null,"\ud800"],"extension":null}}"#,
    );
    let mut h = clock_host(raw.clone());
    let HostReply::ToolResults(results) = h
        .call(
            &control(),
            HostCall::ExecuteBatch {
                calls: vec![invocation("a"), invocation("b")],
                hints: tools(),
            },
        )
        .unwrap()
    else {
        panic!("known failed response must be projected results")
    };
    let message = JsString::from_utf16(vec![120, 44, 44, 0xd800]);
    for (index, result) in results.iter().enumerate() {
        let value = result.boundary.value();
        let error = value.get("error").unwrap();
        assert_eq!(
            error.get("code"),
            Some(&JsValue::from("tool_execution_failed"))
        );
        assert_eq!(
            error.get("message"),
            Some(&JsValue::String(message.clone()))
        );
        assert_eq!(
            error.get("details").unwrap().get("moduleError"),
            raw.get("error")
        );
        assert_eq!(
            error.get("details").unwrap().get("moduleCode"),
            raw.get("code")
        );
        assert_eq!(
            result.started_at,
            JsString::from(format!("2026-10-09T00:00:00.{:03}Z", 2 * index))
        );
        assert_eq!(
            result.completed_at,
            JsString::from(format!("2026-10-09T00:00:00.{:03}Z", 2 * index + 1))
        );
        assert_eq!(
            result.tool_call_id,
            JsString::from(if index == 0 { "a" } else { "b" })
        );
        assert!(value.get("metadata").is_none());
        let Some(JsValue::Array(content)) = value.get("content") else {
            panic!()
        };
        assert_eq!(
            content[0].get("text"),
            Some(&JsValue::String(message.clone()))
        );
    }
    let exchange = h.into_exchange();
    assert_eq!(exchange.samples.get(), 4);
    assert_eq!(exchange.inner.calls.len(), 1);
    assert_eq!(exchange.inner.calls[0].2, CallEffect::SideEffect);
}

#[test]
fn failed_batch_maps_known_permission_code_and_omits_falsy_module_details() {
    for (extra, code, details) in [
        (r#", "code":"permission_denied""#, "permission_denied", true),
        (
            r#", "code":"", "error":null"#,
            "tool_execution_failed",
            false,
        ),
    ] {
        let raw = json(&format!(
            r#"{{"kind":"response","messageId":"m","inReplyTo":"reply","ok":false{extra}}}"#
        ));
        let mut h = clock_host(raw);
        let HostReply::ToolResults(results) = h
            .call(
                &control(),
                HostCall::ExecuteBatch {
                    calls: vec![invocation("a")],
                    hints: tools(),
                },
            )
            .unwrap()
        else {
            panic!()
        };
        let error = results[0].boundary.value().get("error").unwrap();
        assert_eq!(error.get("code"), Some(&JsValue::from(code)));
        assert_eq!(
            error.get("message"),
            Some(&JsValue::from("Capability module failed."))
        );
        assert_eq!(error.get("details").is_some(), details);
        assert_eq!(h.into_exchange().samples.get(), 2);
    }
}

#[test]
fn unknown_failed_batch_does_not_sample_clocks_synthesize_or_replay() {
    let mut h = clock_host(json(
        r#"{"kind":"response","messageId":"m","inReplyTo":"reply","ok":false,"outcome":"result_unknown","code":"permission_denied"}"#,
    ));
    assert!(matches!(h.call(&control(), HostCall::ExecuteBatch {
        calls: vec![invocation("a"), invocation("b")], hints: tools(),
    }), Err(PortError::AcceptedUnknown { identity, .. }) if *identity == owner()));
    let exchange = h.into_exchange();
    assert_eq!(exchange.samples.get(), 0);
    assert_eq!(exchange.inner.calls.len(), 1);
    assert!(exchange.inner.responses.is_empty());
}
#[test]
fn completed_batch_rejection_preserves_identity_exact_source_message_and_no_replay() {
    for (payload, expected) in [
        (
            r#"{}"#,
            "Capability batch response must contain one result for every tool call.",
        ),
        (
            r#"{"results":[]}"#,
            "Capability batch response must contain one result for every tool call.",
        ),
        (
            r#"{"results":[null]}"#,
            "Capability batch result 0 is invalid.",
        ),
        (
            r#"{"results":[{"type":"success","toolCallId":"other"}]}"#,
            "Capability batch result 0 does not match tool call a.",
        ),
    ] {
        let raw = json(&format!(
            r#"{{"kind":"response","messageId":"m","inReplyTo":"reply","ok":true,"payload":{payload}}}"#
        ));
        let mut h = clock_host(raw.clone());
        let HostReply::ToolBatchRejected {
            message,
            boundary,
            identity,
        } = h
            .call(
                &control(),
                HostCall::ExecuteBatch {
                    calls: vec![invocation("a")],
                    hints: tools(),
                },
            )
            .unwrap()
        else {
            panic!("known completed response must be a consumer rejection")
        };
        assert_eq!(message, JsString::from(expected));
        assert_eq!(*identity, owner());
        assert_eq!(boundary.value(), &raw);
        let exchange = h.into_exchange();
        assert_eq!(exchange.samples.get(), 0);
        assert_eq!(exchange.inner.calls.len(), 1);
        assert!(exchange.inner.responses.is_empty());
    }
}

#[test]
fn completed_batch_id_error_keeps_unpaired_utf16_identifier() {
    let raw = json(
        r#"{"kind":"response","messageId":"m","inReplyTo":"reply","ok":true,"payload":{"results":[{"type":"success","toolCallId":"other"}]}}"#,
    );
    let mut h = clock_host(raw);
    let mut call = invocation("a");
    call.tool_call_id = JsString::from_utf16(vec![0xd800]);
    let HostReply::ToolBatchRejected { message, .. } = h
        .call(
            &control(),
            HostCall::ExecuteBatch {
                calls: vec![call],
                hints: tools(),
            },
        )
        .unwrap()
    else {
        panic!()
    };
    let mut expected = "Capability batch result 0 does not match tool call "
        .encode_utf16()
        .collect::<Vec<_>>();
    expected.extend([0xd800, 46]);
    assert_eq!(message.units(), expected);
}

#[test]
fn stdio_failure_recording_string_exception_maps_plain_transport_results_without_replay() {
    let raw = json(
        r#"{"kind":"response","messageId":"m","inReplyTo":"reply","ok":false,"error":{"message":{"toString":null}}}"#,
    );
    let mut h = clock_host(raw.clone());
    let HostReply::ToolResults(results) = h
        .call(
            &control(),
            HostCall::ExecuteBatch {
                calls: vec![invocation("a"), invocation("b")],
                hints: tools(),
            },
        )
        .unwrap()
    else {
        panic!()
    };
    assert_eq!(results.len(), 2);
    for (result, id) in results.iter().zip(["a", "b"]) {
        let boundary = result.boundary.value();
        assert_eq!(boundary.get("toolCallId"), Some(&JsValue::from(id)));
        assert_eq!(
            boundary.get("error"),
            Some(&json(
                r#"{"code":"tool_execution_failed","message":"Cannot convert object to primitive value","details":{"moduleCode":"MODULE_TRANSPORT_UNAVAILABLE","outcome":"result_unknown","retryability":"unsafe"}}"#
            ))
        );
        assert_eq!(
            boundary.get("content"),
            Some(&json(
                r#"[{"type":"text","text":"Cannot convert object to primitive value"}]"#
            ))
        );
    }
    let exchange = h.into_exchange();
    assert_eq!(exchange.samples.get(), 4);
    assert_eq!(exchange.inner.calls.len(), 1);
}

#[test]
fn malformed_envelope_stays_accepted_unknown_despite_tool_payload() {
    let mut h = clock_host(json(
        r#"{"kind":"response","messageId":"m","inReplyTo":"reply","ok":"yes","payload":{"results":[]}}"#,
    ));
    assert!(
        matches!(h.call(&control(), HostCall::ExecuteBatch { calls: vec![invocation("a")], hints: tools() }), Err(PortError::AcceptedUnknown { identity, .. }) if *identity == owner())
    );
    let exchange = h.into_exchange();
    assert_eq!(exchange.samples.get(), 0);
    assert_eq!(exchange.inner.calls.len(), 1);
}

fn control() -> CallControl {
    CallControl {
        cancellation: Cancellation::default(),
        deadline: None,
    }
}
fn route() -> Route {
    Route {
        provider: "p".into(),
        model: "m".into(),
    }
}
fn request() -> CapturedRequest {
    admit_model_request(&json(r#"{"provider":"p","model":"m","messages":[]}"#)).unwrap()
}
fn execution() -> ExecutionHints {
    ExecutionHints {
        run_id: "r".into(),
        operation_id: "o".into(),
        idempotency_key: None,
        operation_deadline: None,
    }
}
fn hints() -> ModelHints {
    ModelHints {
        session_id: "s".into(),
        turn_id: "t".into(),
        execution: execution(),
        workspace_id: None,
        storage_config_version: None,
        metadata: None,
        model_override: None,
    }
}
fn tools() -> ToolHints {
    ToolHints {
        session_id: "s".into(),
        turn_id: "t".into(),
        cwd: "/owned".into(),
        permission_mode: PermissionMode::BypassPermissions,
        run_mode: None,
        can_prompt: false,
        can_elicit: None,
        additional_working_directories: vec![],
        context_extensions: JsObject::new(),
        execution: execution(),
    }
}
fn handle() -> PreparedRequestHandle {
    PreparedRequestHandle("same-preparation".into())
}
fn invocation(id: &str) -> ToolInvocation {
    ToolInvocation {
        tool_call_id: id.into(),
        name: "read".into(),
        arguments: JsValue::Null,
    }
}
fn descriptor() -> Captured<ToolDescriptor> {
    Captured {
        view: ToolDescriptor {
            name: "read".into(),
            description: "d".into(),
            kind: "builtin".into(),
            input_schema: JsObject::new(),
            read_only: true,
            concurrency_safe: true,
            requires_user_interaction: false,
            required_runtime_capabilities: None,
        },
        boundary: BoundarySnapshot::capture(&JsValue::Null),
    }
}
fn context() -> ContextPrepare {
    ContextPrepare {
        preview_only: Some(false),
        session_id: "s".into(),
        turn_id: "t".into(),
        step_id: Some(0.0),
        cwd: "/owned".into(),
        runtime_context_surface: Some(RuntimeContextSurface::UserMessage),
        route: route(),
        protocol: None,
        supports_prompt_cache: None,
        permission_mode: "default".into(),
        run_mode: None,
        additional_working_directories: vec![],
        messages: vec![],
        tools: vec![],
        custom_system_prompt: Some("".into()),
        append_system_prompt: None,
        max_messages: None,
    }
}
fn error() -> Captured<pilotdeck_protocol::model::ModelError> {
    let e=admit_model_event(&json(r#"{"type":"error","error":{"provider":"p","protocol":"openai","code":"x","message":"bad","retryable":false}}"#)).unwrap();
    let ModelEvent::Error(view) = e.event else {
        panic!()
    };
    Captured {
        view: *view,
        boundary: BoundarySnapshot::capture(e.boundary.value().get("error").unwrap()),
    }
}
fn compact() -> CompactionIntent {
    CompactionIntent {
        session_id: Some("s".into()),
        turn_id: Some("t".into()),
        stage: BudgetStage::Recovery,
        trigger: CompactionTrigger::Reactive,
        messages: vec![],
        max_context_tokens: Some(100.0),
        reserved_output_tokens: None,
        manual_force: None,
        allow_fallback_on_failure: Some(false),
        budget_request: Some(request()),
        preparation: Some(context()),
        prepared_handle: Some(handle()),
        calibration: None,
    }
}
fn result(id: &str) -> JsValue {
    let mut v = json(
        r#"{"type":"success","toolName":"read","content":[],"startedAt":"a","completedAt":"b","metadata":null,"supplementalMessages":null}"#,
    );
    if let JsValue::Object(o) = &mut v {
        o.insert("toolCallId".into(), id.into());
    }
    v
}
fn payload(k: &str, v: JsValue) -> JsValue {
    JsValue::Object(object(&[(k, v)]))
}

#[test]
fn all_twenty_four_tags_effects_and_reply_associations_are_exercised() {
    let budget = json(
        r#"{"tokens":1,"maxContextTokens":100,"warningRatio":0.7,"blockingRatio":0.9,"ratio":0.01,"state":"ok"}"#,
    );
    let todo = json(
        r#"{"requiresInitialization":false,"toolCallsSinceLastTodoWrite":0,"todos":[],"activeTodos":[],"todoHistory":[],"todoDiagnostics":{"writeCount":0,"todoCount":0,"activeCount":0,"completedCount":0,"cancelledCount":0,"largeRewriteCount":0,"deletedOpenItemCount":0,"completedWithoutActiveCount":0}}"#,
    );
    let ctx = json(
        r#"{"messages":[],"systemPromptParts":[],"tools":[],"diagnostics":[],"boundaries":[]}"#,
    );
    let mut prepared = json(
        r#"{"provider":"p","model":"m","request":{"provider":"p","model":"m","messages":[]}}"#,
    );
    if let JsValue::Object(o) = &mut prepared {
        o.insert("maxContextTokens".into(), JsValue::Number(1.5));
    }
    let cases: Vec<(HostCall, JsValue)> = vec![
        (
            HostCall::ModelMetadata { route: route() },
            payload(
                "metadata",
                json(r#"{"provider":"p","model":"m","tokenLimits":null}"#),
            ),
        ),
        (
            HostCall::ModelPrepare {
                handle: handle(),
                request: Box::new(request()),
                hints: hints(),
            },
            JsValue::Object(object(&[
                ("prepared", prepared),
                ("metadata", json(r#"{"provider":"p","model":"m"}"#)),
            ])),
        ),
        (
            HostCall::ModelMaterialize {
                handle: handle(),
                request: Box::new(request()),
            },
            payload("request", request().wire.request().clone()),
        ),
        (
            HostCall::ModelPull {
                handle: handle(),
                request: Box::new(request()),
                hints: hints(),
            },
            json(r#"{"events":[],"done":false}"#),
        ),
        (HostCall::ModelClose { handle: handle() }, JsValue::Null),
        (
            HostCall::ModelEagerStream {
                handle: handle(),
                request: None,
                hints: hints(),
            },
            json(r#"{"events":[]}"#),
        ),
        (
            HostCall::BudgetEstimate {
                request: Box::new(request()),
            },
            payload("tokens", JsValue::Number(0.0)),
        ),
        (
            HostCall::BudgetEvaluate {
                request: Box::new(request()),
                options: BudgetOptions {
                    max_context_tokens: 100.0,
                    reserved_output_tokens: None,
                    use_provider_count: Some(false),
                    calibration: None,
                },
            },
            payload("snapshot", budget.clone()),
        ),
        (
            HostCall::BudgetCost {
                usage: None,
                route: route(),
            },
            payload("costUsd", JsValue::Null),
        ),
        (HostCall::TurnDrain, json(r#"{"messages":[]}"#)),
        (
            HostCall::TurnDrainOrClose,
            json(r#"{"messages":[],"closed":true}"#),
        ),
        (
            HostCall::TurnPersistCompaction {
                boundary: CompactBoundary {
                    boundary: BoundarySnapshot::capture(&json(r#"{"type":"compact_boundary"}"#)),
                },
                messages: vec![],
            },
            JsValue::Null,
        ),
        (HostCall::ListTools, json(r#"{"tools":[]}"#)),
        (
            HostCall::ExecuteBatch {
                calls: vec![invocation("a")],
                hints: tools(),
            },
            payload("results", JsValue::Array(vec![result("a")])),
        ),
        (
            HostCall::ExecuteSingle {
                call: invocation("a"),
                hints: tools(),
            },
            result("a"),
        ),
        (
            HostCall::PlanTodo(PlanTodoRequest {
                session_id: "s".into(),
                turn_id: "t".into(),
                mutation: PlanTodoMutation::Read,
            }),
            payload("snapshot", todo),
        ),
        (
            HostCall::DecidePermission {
                tool: descriptor(),
                input: JsValue::Null,
                hints: tools(),
                call_id: None,
            },
            json(r#"{"decision":{"type":"allow","reason":{},"updatedInput":null}}"#),
        ),
        (
            HostCall::PrepareContext(Box::new(context())),
            payload("result", ctx),
        ),
        (
            HostCall::ApplyToolResults(ApplyToolResults {
                session_id: "s".into(),
                turn_id: "t".into(),
                tool_result_message: admit_message(&json(r#"{"role":"user","content":[]}"#))
                    .unwrap(),
                supplemental_messages: None,
                messages: vec![],
            }),
            payload("result", json(r#"{"messages":[],"diagnostics":[]}"#)),
        ),
        (
            HostCall::RecoverModelError(RecoverModelError {
                session_id: "s".into(),
                turn_id: "t".into(),
                error: error(),
                messages: vec![],
                has_attempted_compact: false,
            }),
            payload("result", json(r#"{"type":"give_up","reason":"bad"}"#)),
        ),
        (
            HostCall::CaptureTurn(CaptureTurn {
                session_id: "s".into(),
                turn_id: "t".into(),
                messages: vec![],
                errored: false,
            }),
            payload("result", JsValue::Null),
        ),
        (
            HostCall::AutoCompact(Box::new(compact())),
            payload(
                "result",
                JsValue::Object(object(&[("type", "skipped".into()), ("snapshot", budget)])),
            ),
        ),
        (
            HostCall::DispatchLifecycle {
                event: "stop".into(),
                payload: None,
            },
            payload(
                "result",
                json(
                    r#"{"effects":[],"messages":[],"events":[],"blockingErrors":[],"nonBlockingErrors":[]}"#,
                ),
            ),
        ),
        (
            HostCall::EmitEvent {
                event: BoundarySnapshot::capture(&json(
                    r#"{"type":"assistant_text","sessionId":"s","turnId":"t"}"#,
                )),
            },
            JsValue::Null,
        ),
    ];
    assert_eq!(cases.len(), 24);
    for (call, payload_value) in cases {
        let method = call.method();
        let effect = call.effect();
        let mut h = host(vec![response(payload_value)]);
        let reply = h.call(&control(), call.clone()).unwrap();
        assert!(call.accepts(&reply));
        let e = h.into_exchange();
        assert_eq!(e.calls.len(), 1);
        let (m, op, actual_effect, p) = &e.calls[0];
        assert_eq!((*m, *op), method);
        assert_eq!(*actual_effect, effect);
        assert_eq!(
            p.get("operation").unwrap().as_string().unwrap(),
            &JsString::from(method.1)
        );
        if method.1 == "execute" {
            assert_eq!(p.get("arguments"), Some(&JsValue::Null));
            assert!(matches!(
                p.get("execution").unwrap().get("idempotencyKey"),
                Some(JsValue::Undefined)
            ));
        }
        if method.1 == "stream" {
            assert!(matches!(p.get("request"), Some(JsValue::Undefined)));
        }
        if method.1 == "prepare_for_model" {
            assert_eq!(
                p.get("input").unwrap().get("customSystemPrompt"),
                Some(&JsValue::from(""))
            );
        }
    }
}

#[test]
fn unknown_outcome_uses_bound_owner_identity_and_never_retries() {
    let mut raw = json(
        r#"{"kind":"response","messageId":"m","inReplyTo":"untrusted","ok":false,"outcome":"result_unknown","payload":{"requestId":"attacker","sessionId":"attacker"},"lastAppliedSequence":9007199254740992}"#,
    );
    let mut h = host(vec![raw.clone()]);
    let error = h
        .call(
            &control(),
            HostCall::ModelPull {
                handle: handle(),
                request: Box::new(request()),
                hints: hints(),
            },
        )
        .unwrap_err();
    let PortError::AcceptedUnknown {
        identity,
        last_applied_sequence,
    } = error
    else {
        panic!("{error:?}")
    };
    assert_eq!(*identity, owner());
    assert_eq!(last_applied_sequence, Some(9007199254740992.0));
    assert_eq!(h.into_exchange().calls.len(), 1);
    if let JsValue::Object(o) = &mut raw {
        o.insert("lastAppliedSequence".into(), JsValue::Number(-1.0));
    }
    let mut h = host(vec![raw]);
    assert!(h.call(&control(), HostCall::TurnDrain).is_err());
    assert_eq!(h.into_exchange().calls.len(), 1);
    let mut value = result("a");
    if let JsValue::Object(o) = &mut value {
        o.insert("type".into(), "error".into());
        o.insert("error".into(),json(r#"{"code":"HOST_TRANSPORT_ERROR","message":"unknown","details":{"outcome":"result_unknown"}}"#));
    }
    let mut h = host(vec![response(payload(
        "results",
        JsValue::Array(vec![value]),
    ))]);
    assert!(matches!(
        h.call(
            &control(),
            HostCall::ExecuteBatch {
                calls: vec![invocation("a")],
                hints: tools()
            }
        ),
        Err(PortError::AcceptedUnknown { .. })
    ));
    assert_eq!(h.into_exchange().calls.len(), 1);
}

#[test]
fn module_error_precedes_payload_shape_and_shallow_provider_errors_are_preserved() {
    let raw = json(
        r#"{"kind":"response","messageId":"m","inReplyTo":"reply","ok":false,"code":"HOST","payload":17,"error":{"message":"bad","canonical":{"code":"RATE","message":"wait","extension":{"n":null}}}}"#,
    );
    let canonical = raw.get("error").unwrap().get("canonical").unwrap().clone();
    let mut h = host(vec![raw]);
    let error = h
        .call(
            &control(),
            HostCall::ModelMaterialize {
                handle: handle(),
                request: Box::new(request()),
            },
        )
        .unwrap_err();
    let PortError::ProviderBoundaryOnly {
        code,
        message,
        boundary,
    } = error
    else {
        panic!("{error:?}")
    };
    assert_eq!(code, JsString::from("RATE"));
    assert_eq!(message, JsString::from("wait"));
    assert_eq!(boundary.value(), &canonical);
    assert!(boundary.value().get("provider").is_none());
    assert!(boundary.value().get("retryable").is_none());
    let raw = json(
        r#"{"kind":"response","messageId":"m","inReplyTo":"reply","ok":false,"error":{"canonical":{"provider":"p","protocol":"google","code":"X","message":"bad","retryable":false,"retryAfterMs":0,"raw":null}}}"#,
    );
    let mut h = host(vec![raw]);
    assert!(matches!(
        h.call(&control(), HostCall::ModelClose { handle: handle() }),
        Err(PortError::Provider(_))
    ));
    let raw = json(
        r#"{"kind":"response","messageId":"m","inReplyTo":"reply","ok":false,"error":{"message":["x",null,"\ud800"],"canonical":{"code":1,"message":"bad"}},"payload":null}"#,
    );
    let mut h = host(vec![raw.clone()]);
    let error = h.call(&control(), HostCall::TurnDrain).unwrap_err();
    let PortError::HostFailure {
        message, boundary, ..
    } = error
    else {
        panic!()
    };
    assert_eq!(message.units(), &[120, 44, 44, 0xd800]);
    assert_eq!(boundary.value(), &raw);
}

#[test]
fn batches_require_ordered_ids_and_single_execute_has_the_required_tag() {
    for values in [vec![result("b"), result("a")], vec![result("a")]] {
        let mut h = host(vec![response(payload("results", JsValue::Array(values)))]);
        assert!(matches!(
            h.call(
                &control(),
                HostCall::ExecuteBatch {
                    calls: vec![invocation("a"), invocation("b")],
                    hints: tools()
                }
            ),
            Ok(HostReply::ToolBatchRejected { .. })
        ));
        assert_eq!(h.into_exchange().calls.len(), 1);
    }
    let mut h = host(vec![response(result("wrong"))]);
    assert!(matches!(
        h.call(
            &control(),
            HostCall::ExecuteSingle {
                call: invocation("a"),
                hints: tools()
            }
        ),
        Err(PortError::AcceptedUnknown { .. })
    ));
    let e = h.into_exchange();
    assert_eq!(
        e.calls[0].3.get("operation"),
        Some(&JsValue::from("execute"))
    );
    let mut h = host(vec![response(result("a"))]);
    let HostReply::ToolResult(result) = h
        .call(
            &control(),
            HostCall::ExecuteSingle {
                call: invocation("a"),
                hints: tools(),
            },
        )
        .unwrap()
    else {
        panic!()
    };
    assert!(result.metadata.is_none());
    assert!(result.supplemental_messages.is_none());
    assert_eq!(
        result.boundary.value().get("metadata"),
        Some(&JsValue::Null)
    );
    assert_eq!(
        result.boundary.value().get("supplementalMessages"),
        Some(&JsValue::Null)
    );
}

#[test]
fn prepared_lazy_pull_and_close_keep_same_handle_and_raw_request() {
    let mut raw = json(
        r#"{"provider":"p","model":"m","messages":[{"role":"user","content":[{"type":"text"}]}],"extension":{"negativeZero":-0,"lone":"\ud800"},"metadata":null}"#,
    );
    if let JsValue::Object(o) = &mut raw {
        o.insert("ownUndefined".into(), JsValue::Undefined);
    }
    let captured = admit_model_request(&raw).unwrap();
    assert!(matches!(
        captured.normalized,
        RequestProjection::BoundaryOnly(_)
    ));
    let mut h = host(vec![
        response(json(r#"{"events":[],"done":false}"#)),
        response(json(r#"{"events":[],"done":true}"#)),
        response(JsValue::Null),
    ]);
    for _ in 0..2 {
        h.call(
            &control(),
            HostCall::ModelPull {
                handle: handle(),
                request: Box::new(captured.clone()),
                hints: hints(),
            },
        )
        .unwrap();
    }
    h.call(&control(), HostCall::ModelClose { handle: handle() })
        .unwrap();
    let e = h.into_exchange();
    assert_eq!(e.calls.len(), 3);
    for (_, op, _, p) in &e.calls {
        assert_eq!(
            p.get("preparationId"),
            Some(&JsValue::from("same-preparation"))
        );
        assert_ne!(*op, "start_stream");
        if *op == "stream_next" {
            assert_eq!(p.get("request"), Some(&raw));
            assert!(
                p.get("request")
                    .unwrap()
                    .get("extension")
                    .unwrap()
                    .get("negativeZero")
                    .unwrap()
                    .as_number()
                    .unwrap()
                    .is_sign_negative()
            );
        } else {
            assert_eq!(p.len(), 2);
        }
    }
}

#[test]
fn unknown_live_events_and_lax_known_error_have_distinct_preserved_boundaries() {
    let events = json(
        r#"[{"type":"future_event","data":{"x":null}},{"type":"text_delta","text":"\ud800"},{"type":"error","error":{"code":"X","message":"bad"}},{"type":"error","error":{"provider":"p","protocol":"openai","code":"Y","message":"bad","retryable":true}}]"#,
    );
    let mut h = host(vec![response(JsValue::Object(object(&[
        ("events", events.clone()),
        ("done", JsValue::Bool(false)),
    ])))]);
    let HostReply::ModelChunk(chunk) = h
        .call(
            &control(),
            HostCall::ModelPull {
                handle: handle(),
                request: Box::new(request()),
                hints: hints(),
            },
        )
        .unwrap()
    else {
        panic!()
    };
    assert_eq!(chunk.events.len(), 4);
    let JsValue::Array(ref original) = events else {
        panic!()
    };
    assert!(matches!(&chunk.events[0],StreamEvent::BoundaryOnly(b) if b.value()==&original[0]));
    assert!(
        matches!(&chunk.events[1],StreamEvent::Known(e) if matches!(&e.event,ModelEvent::TextDelta{text,..} if text.units()==[0xd800]))
    );
    assert!(
        matches!(&chunk.events[2],StreamEvent::ErrorBoundaryOnly{boundary,error} if boundary.value()==&original[2] && error.value()==original[2].get("error").unwrap())
    );
    assert!(
        matches!(&chunk.events[3],StreamEvent::Known(e) if matches!(&e.event,ModelEvent::Error(_)))
    );
}

#[test]
fn source_budget_cost_keeps_usage_extensions_and_absence_at_same_wire_method() {
    let original = json(r#"{"futureFinite":-0,"nativeCost":0.5,"details":{"provider":"opaque"}}"#);
    let mut h = host(vec![
        response(payload("costUsd", JsValue::Number(0.0))),
        response(payload("costUsd", JsValue::Null)),
    ]);
    let call = HostCall::BudgetCostSnapshot {
        usage: Some(BoundarySnapshot::capture(&original)),
        route: route(),
    };
    assert!(matches!(
        h.call(&control(), call),
        Ok(HostReply::BudgetCost(Some(0.0)))
    ));
    assert!(matches!(
        h.call(
            &control(),
            HostCall::BudgetCostSnapshot {
                usage: None,
                route: route()
            }
        ),
        Ok(HostReply::BudgetCost(None))
    ));
    let exchange = h.into_exchange();
    assert_eq!(exchange.calls.len(), 2);
    for (module, operation, effect, _) in &exchange.calls {
        assert_eq!(
            (*module, *operation, *effect),
            ("budget", "estimate_usage_cost", CallEffect::ReadOnly)
        );
    }
    assert_eq!(exchange.calls[0].3.get("usage"), Some(&original));
    assert!(
        exchange.calls[0]
            .3
            .get("usage")
            .unwrap()
            .get("futureFinite")
            .unwrap()
            .as_number()
            .unwrap()
            .is_sign_negative()
    );
    assert_eq!(exchange.calls[1].3.get("usage"), Some(&JsValue::Undefined));
}

#[test]
fn budget_null_cost_is_absent_zero_is_present_and_optional_extensions_remain_raw() {
    let mut h = host(vec![
        response(payload("costUsd", JsValue::Null)),
        response(payload("costUsd", JsValue::Number(0.0))),
    ]);
    assert!(matches!(
        h.call(
            &control(),
            HostCall::BudgetCost {
                usage: None,
                route: route()
            }
        ),
        Ok(HostReply::BudgetCost(None))
    ));
    assert!(matches!(
        h.call(
            &control(),
            HostCall::BudgetCost {
                usage: Some(Usage::default()),
                route: route()
            }
        ),
        Ok(HostReply::BudgetCost(Some(0.0)))
    ));
    let snapshot = json(
        r#"{"tokens":0,"maxContextTokens":100,"warningRatio":0.7,"blockingRatio":0.9,"ratio":0,"state":"ok","estimateSource":"future","localEstimateTokens":null,"breakdown":{"unknown":true},"reservedOutputTokens":-1}"#,
    );
    let call = HostCall::BudgetEvaluate {
        request: Box::new(request()),
        options: BudgetOptions {
            max_context_tokens: 100.0,
            reserved_output_tokens: None,
            use_provider_count: None,
            calibration: None,
        },
    };
    let mut h = host(vec![response(payload("snapshot", snapshot.clone()))]);
    let HostReply::BudgetSnapshot(s) = h.call(&control(), call.clone()).unwrap() else {
        panic!()
    };
    assert_eq!(s.boundary.value(), &snapshot);
    assert!(s.view.estimate_source.is_none());
    assert!(s.view.local_estimate_tokens.is_none());
    assert!(s.view.breakdown.is_none());
    assert_eq!(s.view.reserved_output_tokens, Some(-1.0));
    for bad in [-1.0, f64::INFINITY, f64::NAN] {
        let mut snapshot = snapshot.clone();
        if let JsValue::Object(o) = &mut snapshot {
            o.insert("tokens".into(), JsValue::Number(bad));
        }
        let mut h = host(vec![response(payload("snapshot", snapshot))]);
        assert!(matches!(
            h.call(&control(), call.clone()),
            Err(PortError::InvalidResponse { .. })
        ));
    }
}

#[test]
fn admission_preserves_lax_media_metadata_and_all_nine_canonical_content_variants() {
    let raw = json(
        r#"{"role":"assistant","content":[{"type":"text"},{"type":"future_block","data":null}],"metadata":null}"#,
    );
    let msg = admit_message(&raw).unwrap();
    assert!(matches!(msg.normalized, MessageProjection::BoundaryOnly(_)));
    assert_eq!(msg.boundary.value(), &raw);
    let raw = json(
        r#"{"role":"assistant","content":[{"type":"text","text":"\ud800"},{"type":"thinking","text":"r","signature":"sig","reasoningContent":"echo"},{"type":"image","source":"url","data":"https://example.invalid/image","mimeType":"image/png","detail":"high"},{"type":"pdf","source":"base64","data":"synthetic","mimeType":"application/pdf","bytes":0},{"type":"audio","source":"base64","data":"synthetic","mimeType":"audio/wav"},{"type":"tool_call","id":"a","name":"read","raw":null},{"type":"tool_result","toolCallId":"a","content":[{"type":"text","text":"done"}]},{"type":"tool_result_reference","toolCallId":"a","path":"/must-not-read","originalBytes":2,"preview":"x","hasMore":true},{"type":"media_reference","path":"/must-not-read","originalBytes":2,"preview":"x","hasMore":true,"mimeType":"image/png","mediaType":"image"}],"metadata":null}"#,
    );
    let msg = admit_message(&raw).unwrap();
    let MessageProjection::Complete(view) = msg.normalized else {
        panic!()
    };
    assert_eq!(view.content.len(), 9);
    assert!(view.metadata.is_none());
    assert!(matches!(&view.content[0].body,ContentBody::Text(t) if t.text.units()==[0xd800]));
    assert!(
        matches!(&view.content[5].body,ContentBody::ToolCall(t) if t.input==JsValue::Undefined && t.raw.as_ref().unwrap().0.as_ref()==&JsValue::Null)
    );
    assert_eq!(msg.boundary.value(), &raw);
    let value = json(
        r#"{"type":"success","toolCallId":"a","toolName":"read","content":[{"type":"future","extension":null},{"type":"json"},{"type":"text","text":"\ud800"}],"startedAt":"a","completedAt":"b","metadata":null,"supplementalMessages":null}"#,
    );
    let result = admit_tool_result(&value).unwrap();
    assert!(matches!(
        &result.content[0],
        ToolContent::BoundaryOnly { .. }
    ));
    assert!(matches!(
        &result.content[1],
        ToolContent::Json {
            value: JsValue::Undefined,
            ..
        }
    ));
    assert!(matches!(&result.content[2],ToolContent::Text{text,..} if text.units()==[0xd800]));
    assert_eq!(result.boundary.value(), &value);
}

#[test]
fn absent_methods_and_cancelled_control_do_not_dispatch() {
    let mut h = RemoteHost::new(
        Exchange {
            responses: vec![],
            calls: vec![],
        },
        AdvertisedMethods { modules: vec![] },
    );
    assert!(matches!(
        h.call(&control(), HostCall::TurnDrain),
        Err(PortError::NotAdvertised { .. })
    ));
    assert!(h.into_exchange().calls.is_empty());
    let mut h = host(vec![]);
    let c = control();
    c.cancellation.cancel(CancelCause::OwnerRetired);
    assert!(matches!(
        h.call(&c, HostCall::TurnDrain),
        Err(PortError::Cancelled {
            cause: CancelCause::OwnerRetired
        })
    ));
    assert!(h.into_exchange().calls.is_empty());
}

#[test]
fn metadata_checks_route_and_safe_positive_limits_but_prepared_limits_are_filtered() {
    for raw in [
        r#"{"provider":"other","model":"m"}"#,
        r#"{"provider":"p","model":"m","maxContextTokens":1.5}"#,
        r#"{"provider":"p","model":"m","maxContextTokens":9007199254740992}"#,
        r#"{"provider":"p","model":"m","tokenLimits":{"maxContextTokens":0}}"#,
    ] {
        let mut h = host(vec![response(payload("metadata", json(raw)))]);
        assert!(matches!(
            h.call(&control(), HostCall::ModelMetadata { route: route() }),
            Err(PortError::InvalidResponse { .. })
        ));
    }
    let prepared = json(
        r#"{"provider":"p","model":"m","request":{"provider":"p","model":"m","messages":[]},"maxContextTokens":null,"maxOutputTokens":1.5}"#,
    );
    let mut h = host(vec![response(JsValue::Object(object(&[
        ("prepared", prepared),
        ("metadata", json(r#"{"provider":"p","model":"m"}"#)),
    ])))]);
    let HostReply::ModelPrepared(p) = h
        .call(
            &control(),
            HostCall::ModelPrepare {
                handle: handle(),
                request: Box::new(request()),
                hints: hints(),
            },
        )
        .unwrap()
    else {
        panic!()
    };
    assert!(p.max_context_tokens.is_none());
    assert!(p.max_output_tokens.is_none());
    assert_eq!(p.handle, handle());
}

#[test]
fn permission_keeps_false_zero_null_and_array_reason_while_rejecting_null_reason() {
    for updated in [JsValue::Bool(false), JsValue::Number(0.0), JsValue::Null] {
        let d = JsValue::Object(object(&[
            ("type", "allow".into()),
            ("reason", JsValue::Array(vec![])),
            ("updatedInput", updated.clone()),
        ]));
        let mut h = host(vec![response(payload("decision", d.clone()))]);
        let HostReply::Permission(p) = h
            .call(
                &control(),
                HostCall::DecidePermission {
                    tool: descriptor(),
                    input: JsValue::Null,
                    hints: tools(),
                    call_id: None,
                },
            )
            .unwrap()
        else {
            panic!()
        };
        assert!(
            matches!(p.view,PermissionDecision::Allow{updated_input:Some(ref v),..} if v==&updated)
        );
        assert_eq!(p.boundary.value(), &d);
    }
    let mut h = host(vec![response(json(
        r#"{"decision":{"type":"allow","reason":null}}"#,
    ))]);
    assert!(matches!(
        h.call(
            &control(),
            HostCall::DecidePermission {
                tool: descriptor(),
                input: JsValue::Null,
                hints: tools(),
                call_id: None
            }
        ),
        Err(PortError::AcceptedUnknown { .. })
    ));
}

#[test]
fn context_result_presence_distinguishes_missing_from_own_undefined_and_persist_ack_is_only_ack() {
    let call = HostCall::CaptureTurn(CaptureTurn {
        session_id: "s".into(),
        turn_id: "t".into(),
        messages: vec![],
        errored: false,
    });
    let mut h = host(vec![
        response(JsValue::Object(JsObject::new())),
        response(payload("result", JsValue::Undefined)),
        response(payload("result", JsValue::Null)),
    ]);
    assert!(h.call(&control(), call.clone()).is_err());
    assert!(matches!(
        h.call(&control(), call.clone()),
        Ok(HostReply::CapturedTurn)
    ));
    assert!(matches!(
        h.call(&control(), call),
        Ok(HostReply::CapturedTurn)
    ));
    let raw = response(JsValue::Null);
    let mut h = host(vec![raw.clone()]);
    let HostReply::CompactionCommitted(commit) = h
        .call(
            &control(),
            HostCall::TurnPersistCompaction {
                boundary: CompactBoundary {
                    boundary: BoundarySnapshot::capture(&JsValue::Null),
                },
                messages: vec![],
            },
        )
        .unwrap()
    else {
        panic!()
    };
    assert_eq!(commit.response.value(), &raw);
    assert!(commit.response.value().get("persisted").is_none());
}

#[test]
fn single_tool_cancellation_matches_original_regex_boundaries_and_code_precedence() {
    for (code, message, cancelled) in [
        ("ABORTED", "", true),
        ("custom cancellation code", "", true),
        ("X", "user CANCELLED", true),
        ("X", "uncancelled", false),
        ("X", "cancellation_id", false),
        ("X", "cancelation", false),
        ("X", "cancel", true),
        ("X", "cancelledé", true),
    ] {
        let raw = JsValue::Object(object(&[
            ("kind", "response".into()),
            ("messageId", "m".into()),
            ("inReplyTo", "reply".into()),
            ("ok", JsValue::Bool(false)),
            ("code", code.into()),
            (
                "error",
                JsValue::Object(object(&[
                    ("code", "ABORTED".into()),
                    ("message", message.into()),
                ])),
            ),
        ]));
        let mut h = host(vec![raw]);
        let outcome = h.call(
            &control(),
            HostCall::ExecuteSingle {
                call: invocation("a"),
                hints: tools(),
            },
        );
        assert_eq!(
            matches!(outcome, Err(PortError::Cancelled { .. })),
            cancelled,
            "{code:?}/{message:?}: {outcome:?}"
        );
    }
    let mut v = result("a");
    if let JsValue::Object(o) = &mut v {
        o.insert(
            "error".into(),
            json(r#"{"code":"CANCELLED","message":"cancelled"}"#),
        );
    }
    let mut h = host(vec![response(v)]);
    assert!(matches!(
        h.call(
            &control(),
            HostCall::ExecuteSingle {
                call: invocation("a"),
                hints: tools()
            }
        ),
        Ok(HostReply::ToolResult(_))
    ));
}

#[test]
fn plan_todo_replays_history_ignores_stale_outer_lists_and_recomputes_history_diagnostics() {
    let raw = json(
        r#"{"approvedPlan":"\ufeff Plan \ufeff","requiresInitialization":false,"toolCallsSinceLastTodoWrite":2,"todos":[{"broken":true}],"activeTodos":[{"broken":true}],"todoHistory":[{"createdAt":"a","mode":"structured","merge":false,"reason":"  start  ","todos":[{"id":"a","content":"open","status":"in_progress"}],"diagnostics":{"stale":true}},{"createdAt":"b","mode":"structured","merge":false,"todos":[{"id":"b","content":"done","status":"completed"}],"diagnostics":null}],"todoDiagnostics":{"writeCount":40,"todoCount":8,"activeCount":3,"completedCount":4,"cancelledCount":0,"largeRewriteCount":9,"deletedOpenItemCount":7,"completedWithoutActiveCount":6}}"#,
    );
    let mut h = host(vec![response(payload("snapshot", raw.clone()))]);
    let HostReply::PlanTodo(v) = h
        .call(
            &control(),
            HostCall::PlanTodo(PlanTodoRequest {
                session_id: "s".into(),
                turn_id: "t".into(),
                mutation: PlanTodoMutation::Read,
            }),
        )
        .unwrap()
    else {
        panic!()
    };
    assert_eq!(v.boundary.value(), &raw);
    assert_eq!(v.view.approved_plan, Some("Plan".into()));
    assert_eq!(v.view.todos.len(), 1);
    assert_eq!(v.view.todos[0].id, Some("b".into()));
    assert!(v.view.active_todos.is_empty());
    assert_eq!(v.view.todo_diagnostics.view.write_count, 40.0);
    let history = &v.view.todo_history;
    assert_eq!(history[0].view.reason, Some("start".into()));
    let d = &history[1].view.diagnostics;
    assert_eq!(d.view.write_count, 2.0);
    assert_eq!(d.view.large_rewrite_count, 1.0);
    assert_eq!(d.view.deleted_open_item_count, 1.0);
    assert_eq!(d.view.completed_without_active_count, 1.0);
    assert_eq!(
        d.boundary.value().get("writeCount"),
        Some(&JsValue::Number(2.0))
    );
    assert_eq!(
        history[1].boundary.value().get("diagnostics"),
        Some(d.boundary.value())
    );
}

#[test]
fn full_request_fields_cache_echo_and_numeric_coordinates_are_not_lost() {
    let raw = json(
        r#"{"provider":"p","model":"m","messages":[{"role":"assistant","content":[{"type":"text","text":"x","timeline":{"version":1,"turnId":"t","id":"b","order":9007199254740992,"revision":0,"offset":2}}],"metadata":{"moduleId":"mod","trustedContext":{"source":"host","purpose":"skill_context","scope":"turn","authorizedPrincipal":"p"},"forkCarryover":{"sourceSessionId":"old"}}}],"systemPrompt":"","tools":[{"name":"read","description":"","inputSchema":{"unknown":null}}],"toolChoice":{"type":"tool","name":"read"},"maxOutputTokens":0,"temperature":0,"speed":-0,"thinking":{"enabled":true,"mode":"xhigh","budgetTokens":0,"preserve":false,"splitReasoning":true},"stream":false,"metadata":{"extension":null},"outputSchema":{"name":"o","description":"","schema":{},"strict":false},"cacheBreakpoints":[0,1.5],"cachePlan":{"provider":"p","model":"m","system":false,"tools":true,"messages":[0],"fingerprint":"f","generation":9007199254740992},"extension":{"own":null}}"#,
    );
    let request = admit_model_request(&raw).unwrap();
    assert_eq!(request.wire.request(), &raw);
    let RequestProjection::Complete(r) = request.normalized else {
        panic!()
    };
    assert_eq!(r.system_prompt, Some("".into()));
    assert_eq!(r.tools.as_ref().unwrap()[0].description, Some("".into()));
    assert_eq!(r.max_output_tokens, Some(0.0));
    assert!(r.speed.unwrap().is_sign_negative());
    assert_eq!(r.thinking.as_ref().unwrap().budget_tokens, Some(0.0));
    assert_eq!(r.stream, Some(false));
    assert_eq!(r.cache_breakpoints, Some(vec![0.0, 1.5]));
    assert_eq!(r.cache_plan.unwrap().generation, 9007199254740992.0);
    assert_eq!(
        r.messages[0].content[0].timeline.as_ref().unwrap().order,
        9007199254740992.0
    );
    assert!(
        r.messages[0]
            .metadata
            .as_ref()
            .unwrap()
            .trusted_context
            .is_some()
    );
}

#[test]
fn context_normalized_projection_retains_materialization_diagnostics_cache_and_boundary() {
    let raw = json(
        r#"{"messages":[{"role":"user","content":[{"type":"text"}]}],"systemPrompt":"","systemPromptParts":[""],"tools":[],"diagnostics":[{"code":"X","severity":"fatal","message":"bad","path":"/do-not-read"}],"boundaries":[{"type":"microcompact","retainedMessages":2,"metadata":null}],"metadata":null,"materialization":{"stepId":0,"promptGeneration":1,"runtimeContexts":[{"name":"s","text":"\ud800"}],"runtimeContextMessages":[],"instructionLayers":[{"scope":"project","path":"/do-not-read","content":"x"}]},"cacheBreakpoints":[1],"cachePlan":{"system":true,"tools":false,"messages":[],"fingerprint":"x","generation":1},"extension":{"keep":null}}"#,
    );
    let mut h = host(vec![response(payload("result", raw.clone()))]);
    let HostReply::Context(v) = h
        .call(&control(), HostCall::PrepareContext(Box::new(context())))
        .unwrap()
    else {
        panic!()
    };
    assert_eq!(v.boundary.value(), &raw);
    assert_eq!(v.view.system_prompt, Some("".into()));
    assert!(matches!(
        v.view.messages[0].normalized,
        MessageProjection::BoundaryOnly(_)
    ));
    assert_eq!(v.view.diagnostics[0].severity, DiagnosticSeverity::Fatal);
    assert!(v.view.boundaries[0].metadata.is_none());
    assert!(v.view.metadata.is_none());
    let material = v.view.materialization.unwrap();
    assert_eq!(material.runtime_contexts[0].text.units(), &[0xd800]);
    assert_eq!(
        material.instruction_layers.unwrap()[0].path,
        JsString::from("/do-not-read")
    );
    assert!(v.view.cache_plan.unwrap().system);
}

#[test]
fn lifecycle_all_known_effects_have_typed_views_and_opaque_values_are_preserved() {
    let raw = json(
        r#"{"effects":[{"type":"additional_context","content":"x","source":"host"},{"type":"system_message","content":"s"},{"type":"block","reason":"b","stopReason":"stop"},{"type":"permission_decision","behavior":"allow","reason":"r"},{"type":"updated_tool_input","input":{"x":null}},{"type":"updated_mcp_tool_output","output":false},{"type":"permission_request_result","result":{"foo":null}},{"type":"initial_user_message","message":"i"},{"type":"watch_paths","paths":["/do-not-read"]},{"type":"worktree_path","path":"/owned"},{"type":"retry_permission_denied"}],"messages":[null],"events":[{"unknown":null}],"blockingErrors":[],"nonBlockingErrors":[],"pendingAsyncHooks":[false],"extension":"keep"}"#,
    );
    let mut h = host(vec![response(payload("result", raw.clone()))]);
    let HostReply::Lifecycle(v) = h
        .call(
            &control(),
            HostCall::DispatchLifecycle {
                event: "pre_tool_use".into(),
                payload: Some(JsObject::new()),
            },
        )
        .unwrap()
    else {
        panic!()
    };
    assert_eq!(v.boundary.value(), &raw);
    assert_eq!(v.view.effects.len(), 11);
    assert!(matches!(
        &v.view.effects[5],
        HookEffect::UpdatedMcpToolOutput {
            output: JsValue::Bool(false)
        }
    ));
    assert_eq!(v.view.messages[0].value(), &JsValue::Null);
    assert_eq!(
        v.view.pending_async_hooks.unwrap()[0].value(),
        &JsValue::Bool(false)
    );
}

#[test]
fn whole_lax_context_values_successfully_admit_without_fake_defaults() {
    let null = JsValue::Null;
    let own_undefined = JsValue::Undefined;
    let object = json(r#"{"unknown":{"x":null}}"#);
    for (call, value) in [
        (HostCall::PrepareContext(Box::new(context())), null),
        (
            HostCall::ApplyToolResults(ApplyToolResults {
                session_id: "s".into(),
                turn_id: "t".into(),
                tool_result_message: admit_message(&json(r#"{"role":"user","content":[]}"#))
                    .unwrap(),
                supplemental_messages: None,
                messages: vec![],
            }),
            object,
        ),
        (
            HostCall::RecoverModelError(RecoverModelError {
                session_id: "s".into(),
                turn_id: "t".into(),
                error: error(),
                messages: vec![],
                has_attempted_compact: false,
            }),
            own_undefined,
        ),
        (
            HostCall::AutoCompact(Box::new(compact())),
            JsValue::Bool(false),
        ),
    ] {
        let mut h = host(vec![response(payload("result", value.clone()))]);
        let reply = h.call(&control(), call.clone()).unwrap();
        assert!(call.accepts(&reply));
        let boundary = match reply {
            HostReply::ContextBoundaryOnly(b)
            | HostReply::ToolResultsAppliedBoundaryOnly(b)
            | HostReply::RecoveryBoundaryOnly(b)
            | HostReply::CompactedBoundaryOnly(b) => b,
            _ => panic!("fabricated normalized result"),
        };
        assert_eq!(boundary.value(), &value);
        assert_eq!(h.into_exchange().calls.len(), 1);
    }
}

#[test]
fn lifecycle_unknown_and_lax_known_effects_are_retained_not_dropped() {
    let raw = json(
        r#"{"effects":[null,{"type":"future","opaque":false},{"type":"block","reason":null},{"type":"system_message","content":"good"}],"messages":[],"events":[],"blockingErrors":[],"nonBlockingErrors":[]}"#,
    );
    let mut h = host(vec![response(payload("result", raw.clone()))]);
    let HostReply::Lifecycle(v) = h
        .call(
            &control(),
            HostCall::DispatchLifecycle {
                event: "stop".into(),
                payload: None,
            },
        )
        .unwrap()
    else {
        panic!()
    };
    let JsValue::Array(effects) = raw.get("effects").unwrap() else {
        panic!()
    };
    assert_eq!(v.view.effects.len(), 4);
    for (index, effect) in v.view.effects.iter().take(3).enumerate() {
        assert!(
            matches!(effect,HookEffect::BoundaryOnly{boundary} if boundary.value()==&effects[index])
        );
    }
    assert!(
        matches!(&v.view.effects[3],HookEffect::SystemMessage{content} if content.equals_str("good"))
    );
    assert_eq!(v.boundary.value(), &raw);
}

#[test]
fn whole_lax_tool_results_keep_every_boundary_but_all_batch_ids_are_checked_first() {
    let first = json(r#"{"type":"future","toolCallId":"a","extension":{"keep":null}}"#);
    let second = result("b");
    let raw = JsValue::Array(vec![first.clone(), second]);
    let mut h = host(vec![response(payload("results", raw.clone()))]);
    let HostReply::ToolResultsBoundaryOnly(values) = h
        .call(
            &control(),
            HostCall::ExecuteBatch {
                calls: vec![invocation("a"), invocation("b")],
                hints: tools(),
            },
        )
        .unwrap()
    else {
        panic!()
    };
    let JsValue::Array(original) = &raw else {
        panic!()
    };
    assert_eq!(values.len(), 2);
    for (b, v) in values.iter().zip(original) {
        assert_eq!(b.value(), v);
    }
    let mut h = host(vec![response(payload(
        "results",
        JsValue::Array(vec![first, result("wrong")]),
    ))]);
    assert!(matches!(
        h.call(
            &control(),
            HostCall::ExecuteBatch {
                calls: vec![invocation("a"), invocation("b")],
                hints: tools()
            }
        ),
        Ok(HostReply::ToolBatchRejected { .. })
    ));
    let raw = json(r#"{"type":"future","extension":null}"#);
    let mut h = host(vec![response(raw.clone())]);
    let HostReply::ToolResultBoundaryOnly(b) = h
        .call(
            &control(),
            HostCall::ExecuteSingle {
                call: invocation("a"),
                hints: tools(),
            },
        )
        .unwrap()
    else {
        panic!()
    };
    assert_eq!(b.value(), &raw);
    assert!(b.value().get("toolCallId").is_none());
}

#[test]
fn metadata_callback_is_required_only_when_advertised_and_must_match_prepared_route() {
    let p = json(
        r#"{"provider":"p","model":"m","request":{"provider":"p","model":"m","messages":[]}}"#,
    );
    let call = HostCall::ModelPrepare {
        handle: handle(),
        request: Box::new(request()),
        hints: hints(),
    };
    let mut h = host(vec![response(payload("prepared", p.clone()))]);
    assert!(matches!(
        h.call(&control(), call.clone()),
        Err(PortError::AcceptedUnknown { .. })
    ));
    let mut h = RemoteHost::new(
        Exchange {
            responses: vec![response(payload("prepared", p.clone()))],
            calls: vec![],
        },
        AdvertisedMethods {
            modules: vec![("model".into(), vec!["prepare".into()])],
        },
    );
    assert!(matches!(
        h.call(&control(), call.clone()),
        Ok(HostReply::ModelPrepared(_))
    ));
    let mut h = host(vec![response(JsValue::Object(object(&[
        ("prepared", p),
        ("metadata", json(r#"{"provider":"wrong","model":"m"}"#)),
    ])))]);
    assert!(matches!(
        h.call(&control(), call),
        Err(PortError::AcceptedUnknown { .. })
    ));
}

#[test]
fn malformed_success_after_accepted_advancing_or_effect_call_is_unknown_not_retryable_error() {
    for call in [
        HostCall::TurnDrain,
        HostCall::ModelPull {
            handle: handle(),
            request: Box::new(request()),
            hints: hints(),
        },
        HostCall::ExecuteSingle {
            call: invocation("a"),
            hints: tools(),
        },
    ] {
        let mut h = host(vec![response(JsValue::Null)]);
        let error = h.call(&control(), call).unwrap_err();
        assert!(matches!(error,PortError::AcceptedUnknown{identity,..} if *identity==owner()));
        assert_eq!(h.into_exchange().calls.len(), 1);
    }
    let mut h = host(vec![response(JsValue::Null)]);
    assert!(matches!(
        h.call(&control(), HostCall::ListTools),
        Err(PortError::InvalidResponse { .. })
    ));
}

#[test]
fn terminal_capability_failure_precedes_record_failure_and_preserves_catch_provenance() {
    let batch = HostCall::ExecuteBatch {
        calls: vec![invocation("a")],
        hints: tools(),
    };
    for (raw, expected_code, expected_module_code, expected_message) in [
        (
            r#"{"kind":"response","messageId":"m","inReplyTo":"reply","ok":false,"final":true,"outcome":"failed","code":"MODULE_TIMEOUT"}"#,
            "tool_timeout",
            "MODULE_TIMEOUT",
            "Host module execution failed.",
        ),
        (
            r#"{"kind":"response","messageId":"m","inReplyTo":"reply","ok":false,"final":true,"outcome":"failed","code":"X","error":{"message":0}}"#,
            "tool_execution_failed",
            "X",
            "0",
        ),
        (
            r#"{"kind":"response","messageId":"m","inReplyTo":"reply","ok":false,"final":true,"outcome":"failed","code":{"toString":1}}"#,
            "tool_execution_failed",
            "MODULE_TRANSPORT_UNAVAILABLE",
            "Host module execution failed.",
        ),
        (
            r#"{"kind":"response","messageId":"m","inReplyTo":"reply","ok":false,"final":true,"outcome":"failed","code":"MODULE_ABORTED","error":{"message":{"toString":1}}}"#,
            "tool_execution_failed",
            "MODULE_TRANSPORT_UNAVAILABLE",
            "Cannot convert object to primitive value",
        ),
    ] {
        let mut h = host(vec![json(raw)]);
        let c = control();
        let HostReply::ToolResults(results) = h.call(&c, batch.clone()).unwrap() else {
            panic!()
        };
        assert!(c.check().is_ok());
        assert_eq!(results.len(), 1);
        let result = results[0].boundary.value();
        let error = result.get("error").unwrap();
        assert_eq!(
            error.get("code"),
            Some(&JsValue::String(expected_code.into()))
        );
        assert_eq!(
            error.get("message"),
            Some(&JsValue::String(expected_message.into()))
        );
        assert_eq!(
            error.get("details").unwrap().get("moduleCode"),
            Some(&JsValue::String(expected_module_code.into()))
        );
        assert_eq!(
            error.get("details").unwrap().get("retryability"),
            Some(&JsValue::String("unsafe".into()))
        );
        assert_eq!(h.into_exchange().calls.len(), 1);
    }
    let mut h = host(vec![json(
        r#"{"kind":"response","messageId":"m","inReplyTo":"reply","ok":false,"final":true,"outcome":"failed","code":"MODULE_ABORTED"}"#,
    )]);
    let c = control();
    assert!(
        matches!(h.call(&c, batch), Ok(HostReply::ToolBatchRejected { message, .. }) if message.equals_str("Tool execution cancelled."))
    );
    assert_eq!(
        c.cancellation.cause(),
        Some(CancelCause::Requested("tool_cancelled".into()))
    );
    assert_eq!(h.into_exchange().calls.len(), 1);
}

#[test]
fn plan_observer_terminal_properties_are_assigned_only_after_successful_string_conversion() {
    let call = HostCall::PlanTodo(PlanTodoRequest {
        session_id: "s".into(),
        turn_id: "t".into(),
        mutation: PlanTodoMutation::Read,
    });
    for raw_code in ["null", r#"{"toString":1}"#, r#""X""#] {
        let raw = json(&format!(
            r#"{{"kind":"response","messageId":"m","inReplyTo":"reply","ok":false,"final":true,"outcome":"failed","code":{raw_code}}}"#
        ));
        let mut h = host(vec![raw.clone()]);
        let Err(PortError::KnownTerminalException {
            message,
            code,
            boundary,
        }) = h.call(&control(), call.clone())
        else {
            panic!()
        };
        assert!(message.equals_str("Host module execution failed."));
        assert_eq!(
            code,
            if raw_code == r#""X""# {
                Some("X".into())
            } else {
                None
            }
        );
        assert_eq!(boundary.value(), &raw);
        assert_eq!(h.into_exchange().calls.len(), 1);
    }
    let mut h = host(vec![json(
        r#"{"kind":"response","messageId":"m","inReplyTo":"reply","ok":false,"final":true,"outcome":"failed","code":"RESULT_UNKNOWN","error":{"message":{"toString":1}}}"#,
    )]);
    assert!(
        matches!(h.call(&control(), call), Err(PortError::KnownConsumerException { message, .. }) if message.equals_str("Cannot convert object to primitive value"))
    );
    assert_eq!(h.into_exchange().calls.len(), 1);
}

#[test]
fn plan_todo_lax_source_valid_snapshot_has_boundary_only_not_fake_diagnostics() {
    let raw = json(
        r#"{"approvedPlan":false,"requiresInitialization":false,"toolCallsSinceLastTodoWrite":0,"todos":[null],"activeTodos":[false],"todoHistory":[{"createdAt":null,"mode":"structured","merge":null,"reason":null,"markdown":null,"todos":[null,{"status":"future","extension":true}]}],"todoDiagnostics":[],"lastMarkdown":null,"extension":{"keep":null}}"#,
    );
    let call = HostCall::PlanTodo(PlanTodoRequest {
        session_id: "s".into(),
        turn_id: "t".into(),
        mutation: PlanTodoMutation::Read,
    });
    let mut h = host(vec![response(payload("snapshot", raw.clone()))]);
    let reply = h.call(&control(), call.clone()).unwrap();
    assert!(call.accepts(&reply));
    let HostReply::PlanTodoBoundaryOnly(b) = reply else {
        panic!("fabricated defaults")
    };
    assert_eq!(b.value(), &raw);
    assert_eq!(
        b.value().get("todoDiagnostics"),
        Some(&JsValue::Array(vec![]))
    );
}

#[test]
fn plan_todo_fallback_does_not_swallow_original_invalid_shape_or_trim_exceptions() {
    let base = json(
        r#"{"requiresInitialization":false,"toolCallsSinceLastTodoWrite":0,"todos":[],"activeTodos":[],"todoHistory":[],"todoDiagnostics":{}}"#,
    );
    let call = HostCall::PlanTodo(PlanTodoRequest {
        session_id: "s".into(),
        turn_id: "t".into(),
        mutation: PlanTodoMutation::Read,
    });
    for (key, value) in [
        ("requiresInitialization", JsValue::Null),
        ("toolCallsSinceLastTodoWrite", JsValue::Number(0.5)),
        ("todoDiagnostics", JsValue::Null),
        ("approvedPlan", JsValue::Number(1.0)),
        (
            "todoHistory",
            json(r#"[{"mode":"structured","todos":[],"reason":false}]"#),
        ),
        ("todoHistory", json(r#"[{"mode":"future","todos":[]}]"#)),
    ] {
        let mut raw = base.clone();
        if let JsValue::Object(o) = &mut raw {
            o.insert(key.into(), value.clone());
        }
        let mut h = host(vec![response(payload("snapshot", raw))]);
        let error = h.call(&control(), call.clone()).unwrap_err();
        // Pinned Source constructs ordinary checkpoint TypeErrors for shapes.
        // Plan/reason trim exceptions remain explicit unrepresented profiles.
        if key == "approvedPlan"
            || matches!(&value, JsValue::Array(items) if items.first().and_then(|item| item.get("reason")).is_some())
        {
            assert!(matches!(error, PortError::InvalidResponse { .. }));
        } else {
            let PortError::KnownConsumerException { message, .. } = error else {
                panic!("{error:?}");
            };
            let expected = if key == "todoHistory" {
                "plan/todo checkpoint history is invalid."
            } else {
                "plan/todo checkpoint is invalid."
            };
            assert_eq!(message, JsString::from(expected));
        }
    }
    let mut h = host(vec![response(payload("snapshot", base))]);
    assert!(matches!(
        h.call(&control(), call),
        Ok(HostReply::PlanTodoBoundaryOnly(_))
    ));
}

#[test]
fn live_catalog_uses_original_defaults_exact_true_flags_and_preserves_entry_boundary() {
    let raw = json(
        r#"[{"name":"x","description":"","kind":"future","readOnly":1,"concurrencySafe":"true","requiresUserInteraction":false,"inputSchema":null,"input_schema":{"type":"object","keep":null},"requiredRuntimeCapabilities":["user_interaction","unknown",null,"user_interaction","plan_workflow"]},{"name":"agent","kind":"agent","requiresUserInteraction":true,"readOnly":true,"concurrencySafe":true},{"name":" \ud800 ","description":"d","inputSchema":{"type":"array"},"input_schema":{"type":"object","ignored":true}}]"#,
    );
    let mut h = host(vec![response(payload("tools", raw.clone()))]);
    let HostReply::Tools(items) = h.call(&control(), HostCall::ListTools).unwrap() else {
        panic!()
    };
    let JsValue::Array(original) = &raw else {
        panic!()
    };
    assert_eq!(items.len(), 3);
    for (item, boundary) in items.iter().zip(original) {
        assert_eq!(item.boundary.value(), boundary);
    }
    assert_eq!(items[0].view.description, JsString::from("Host capability"));
    assert_eq!(items[0].view.kind, JsString::from("custom"));
    assert!(!items[0].view.read_only);
    assert!(!items[0].view.concurrency_safe);
    assert_eq!(items[0].view.input_schema.get("keep"), Some(&JsValue::Null));
    assert_eq!(
        items[0].view.required_runtime_capabilities,
        Some(vec!["user_interaction".into(), "plan_workflow".into()])
    );
    assert_eq!(
        items[1].view.required_runtime_capabilities,
        Some(vec!["subagent_fork".into(), "user_interaction".into()])
    );
    assert!(items[1].view.requires_user_interaction);
    assert!(items[1].view.read_only);
    assert!(items[1].view.concurrency_safe);
    assert_eq!(
        items[1].view.input_schema,
        object(&[("type", "object".into())])
    );
    assert_eq!(items[2].view.name.units(), &[32, 0xd800, 32]);
    assert_eq!(
        items[2].view.input_schema,
        object(&[("type", "object".into())])
    );
    let mut h = host(vec![response(json(
        r#"{"tools":{"available":[{"name":"x"}]}}"#,
    ))]);
    assert!(matches!(
        h.call(&control(), HostCall::ListTools),
        Ok(HostReply::Tools(_))
    ));
}

#[test]
fn live_catalog_rejects_empty_duplicate_utf16_and_nonobject_entries() {
    for raw in [
        r#"[{"name":""}]"#,
        r#"[{"name":"\ud800"},{"name":"\ud800"}]"#,
        r#"[{"name":"x"},{"name":"x"}]"#,
        r#"[null]"#,
        r#"["x"]"#,
        r#"[{"name":0}]"#,
        r#"{"available":null}"#,
    ] {
        let mut h = host(vec![response(payload("tools", json(raw)))]);
        assert!(
            matches!(
                h.call(&control(), HostCall::ListTools),
                Err(PortError::InvalidResponse { .. })
            ),
            "{raw}"
        );
    }
    let mut h = host(vec![response(json(
        r#"{"tools":[{"name":"\ud800"},{"name":"\ud801"}]}"#,
    ))]);
    let HostReply::Tools(items) = h.call(&control(), HostCall::ListTools).unwrap() else {
        panic!()
    };
    assert_eq!(items.len(), 2);
}

#[test]
fn prepared_and_materialized_lax_object_requests_preserve_raw_and_external_route() {
    let prepared =
        json(r#"{"provider":"outer-p","model":"outer-m","request":{"extension":{"raw":null}}}"#);
    let request = prepared.get("request").unwrap().clone();
    let mut h = RemoteHost::new(
        Exchange {
            responses: vec![response(payload("prepared", prepared.clone()))],
            calls: vec![],
        },
        AdvertisedMethods {
            modules: vec![("model".into(), vec!["prepare".into()])],
        },
    );
    let HostReply::ModelPrepared(p) = h
        .call(
            &control(),
            HostCall::ModelPrepare {
                handle: handle(),
                request: Box::new(crate::request()),
                hints: hints(),
            },
        )
        .unwrap()
    else {
        panic!()
    };
    assert_eq!(p.request.route, p.route);
    assert_eq!(
        p.route,
        Route {
            provider: "outer-p".into(),
            model: "outer-m".into()
        }
    );
    assert_eq!(p.request.wire.request(), &request);
    assert!(matches!(
        p.request.normalized,
        RequestProjection::BoundaryOnly(_)
    ));
    assert!(p.request.wire.request().get("messages").is_none());
    assert!(p.request.wire.request().get("provider").is_none());
    for raw in [
        json("{}"),
        json("[]"),
        json(r#"{"unknown":null}"#),
        json(r#"{"provider":"inner","model":"inner","messages":null}"#),
    ] {
        let mut h = host(vec![response(payload("request", raw.clone()))]);
        let HostReply::ModelMaterialized(r) = h
            .call(
                &control(),
                HostCall::ModelMaterialize {
                    handle: handle(),
                    request: Box::new(crate::request()),
                },
            )
            .unwrap()
        else {
            panic!()
        };
        assert_eq!(r.route, route());
        assert_eq!(r.wire.request(), &raw);
        assert!(matches!(r.normalized, RequestProjection::BoundaryOnly(_)));
    }
    for raw in [JsValue::Null, JsValue::Undefined, JsValue::Bool(false)] {
        let mut h = host(vec![response(payload("request", raw))]);
        assert!(matches!(
            h.call(
                &control(),
                HostCall::ModelMaterialize {
                    handle: handle(),
                    request: Box::new(crate::request())
                }
            ),
            Err(PortError::AcceptedUnknown { .. })
        ));
    }
}

fn invocation_prepared() -> PreparedInvocation {
    PreparedInvocation {
        handle: handle(),
        request: request(),
        route: route(),
        max_context_tokens: None,
        max_output_tokens: None,
        metadata: None,
        boundary: BoundarySnapshot::capture(&JsValue::Null),
    }
}

#[test]
fn ignored_known_start_events_and_js_coercible_deltas_keep_original_live_echo_and_effect() {
    use pilotdeck_agent::driver::InvocationDisposition;
    use pilotdeck_agent::source_invocation::run_source_model_invocation;
    let events = json(
        r#"[{"type":"message_start","extension":null},{"type":"tool_call_start"},{"type":"tool_call_delta","id":null},{"type":"text_delta","text":7,"extension":null},{"type":"text_delta","text":null},{"type":"thinking_delta","text":false,"reasoningContent":0,"signature":false},{"type":"message_end","finishReason":"stop"}]"#,
    );
    let mut h = host(vec![response(JsValue::Object(object(&[
        ("events", events.clone()),
        ("done", JsValue::Bool(true)),
    ])))]);
    let mut live = Vec::new();
    let report = run_source_model_invocation(
        &mut h,
        &control(),
        &control(),
        &invocation_prepared(),
        &hints(),
        "response".into(),
        &mut |event| {
            live.push(event);
            Ok(())
        },
    );
    let JsValue::Array(original) = &events else {
        panic!()
    };
    assert_eq!(live.len(), original.len());
    for (event, raw) in live.iter().zip(original) {
        assert_eq!(event.boundary.value(), raw);
    }
    assert_eq!(live[3].block_id, Some("response:text:0".into()));
    assert_eq!(live[5].block_id, Some("response:thinking:0".into()));
    assert!(matches!(
        report.disposition,
        InvocationDisposition::Assembled
    ));
    let assembled = report.assembled.unwrap();
    let JsValue::Array(content) = assembled
        .value()
        .get("message")
        .unwrap()
        .get("content")
        .unwrap()
    else {
        panic!()
    };
    assert_eq!(content[0].get("text"), Some(&JsValue::from("7null")));
    assert_eq!(content[1].get("text"), Some(&JsValue::from("false")));
    assert_eq!(
        content[1].get("reasoningContent"),
        Some(&JsValue::from("0"))
    );
    assert!(content[1].get("signature").is_none());
    assert_eq!(h.into_exchange().calls.len(), 1);
}

#[test]
fn direct_coerced_delta_handles_missing_array_object_and_wtf16_without_stringifying_live_event() {
    let mut first = json(r#"{"type":"text_delta","extension":"keep"}"#);
    let missing = first.clone();
    if let JsValue::Object(o) = &mut first {
        o.insert("text".into(), JsValue::Undefined);
    }
    let events = JsValue::Array(vec![
        missing,
        first.clone(),
        json(r#"{"type":"text_delta","text":["\ud800",null,7]}"#),
        json(r#"{"type":"text_delta","text":{"x":1}}"#),
    ]);
    let mut h = host(vec![response(JsValue::Object(object(&[
        ("events", events.clone()),
        ("done", JsValue::Bool(true)),
    ])))]);
    let HostReply::ModelChunk(chunk) = h
        .call(
            &control(),
            HostCall::ModelPull {
                handle: handle(),
                request: Box::new(request()),
                hints: hints(),
            },
        )
        .unwrap()
    else {
        panic!()
    };
    let expected = [
        JsString::from("undefined"),
        JsString::from("undefined"),
        JsString::from_utf16(vec![0xd800, 44, 44, 55]),
        JsString::from("[object Object]"),
    ];
    let JsValue::Array(original) = &events else {
        panic!()
    };
    for ((event, text), raw) in chunk.events.iter().zip(expected).zip(original) {
        let StreamEvent::CoercedDelta { boundary, reducer } = event else {
            panic!()
        };
        assert_eq!(boundary.value(), raw);
        assert!(matches!(&reducer.event,ModelEvent::TextDelta{text:actual,..} if actual==&text));
        assert_eq!(
            reducer.boundary.value().get("extension"),
            raw.get("extension")
        );
    }
}

#[test]
fn nullish_after_structured_end_stops_before_emission_closes_done_true_and_forbids_tools() {
    use pilotdeck_agent::driver::InvocationDisposition;
    use pilotdeck_agent::source_invocation::run_source_model_invocation;
    for nullish in [JsValue::Null, JsValue::Undefined] {
        let events = JsValue::Array(vec![
            json(r#"{"type":"tool_call_end","toolCall":{"id":"before","name":"read","input":{}}}"#),
            json(r#"{"type":"message_end","finishReason":"stop"}"#),
            nullish.clone(),
            json(r#"{"type":"tool_call_end","toolCall":{"id":"after","name":"read","input":{}}}"#),
            json(r#"{"type":"message_end","finishReason":"stop"}"#),
        ]);
        let mut h = host(vec![
            response(JsValue::Object(object(&[
                ("events", events.clone()),
                ("done", JsValue::Bool(true)),
            ]))),
            response(JsValue::Null),
        ]);
        let mut live = Vec::new();
        let report = run_source_model_invocation(
            &mut h,
            &control(),
            &control(),
            &invocation_prepared(),
            &hints(),
            "response".into(),
            &mut |event| {
                live.push(event);
                Ok(())
            },
        );
        assert_eq!(live.len(), 2);
        assert!(
            matches!(&report.disposition,InvocationDisposition::PortFailure(error) if matches!(error.as_ref(),PortError::SourceException{boundary,..} if boundary.value()==&nullish))
        );
        assert_eq!(report.state.tool_calls.len(), 1);
        assert_eq!(
            report.state.tool_calls[0].value().get("id"),
            Some(&JsValue::from("before"))
        );
        assert!(report.state.has_message_end);
        let e = h.into_exchange();
        assert_eq!(
            e.calls.iter().map(|(_, op, _, _)| *op).collect::<Vec<_>>(),
            ["stream_next", "close_stream"]
        );
    }
}

#[test]
fn text_primitive_conversion_exception_preserves_live_prefix_and_stops_done_true_batch() {
    use pilotdeck_agent::driver::InvocationDisposition;
    use pilotdeck_agent::source_invocation::run_source_model_invocation;
    for delta in [
        json(r#"{"type":"text_delta","text":{"toString":null},"extension":"keep"}"#),
        json(r#"{"type":"text_delta","text":[{"toString":7}]}"#),
        json(r#"{"type":"thinking_delta","text":{"toString":"not callable"}}"#),
    ] {
        let first = json(r#"{"type":"text_delta","text":"before"}"#);
        let events = JsValue::Array(vec![
            first.clone(),
            delta.clone(),
            json(r#"{"type":"tool_call_end","toolCall":{"id":"after","name":"read","input":{}}}"#),
            json(r#"{"type":"message_end","finishReason":"stop"}"#),
        ]);
        let mut h = host(vec![
            response(JsValue::Object(object(&[
                ("events", events),
                ("done", JsValue::Bool(true)),
            ]))),
            response(JsValue::Null),
        ]);
        let mut live = Vec::new();
        let report = run_source_model_invocation(
            &mut h,
            &control(),
            &control(),
            &invocation_prepared(),
            &hints(),
            "response".into(),
            &mut |event| {
                live.push(event);
                Ok(())
            },
        );
        assert_eq!(live.len(), 2);
        assert_eq!(live[0].boundary.value(), &first);
        assert_eq!(live[1].boundary.value(), &delta);
        assert!(
            matches!(&report.disposition,InvocationDisposition::PortFailure(error) if matches!(error.as_ref(),PortError::SourceException{message,boundary} if message.equals_str("Cannot convert object to primitive value") && boundary.value()==&delta))
        );
        let assembled = report.assembled.unwrap();
        let JsValue::Array(content) = assembled
            .value()
            .get("message")
            .unwrap()
            .get("content")
            .unwrap()
        else {
            panic!()
        };
        assert_eq!(content[0].get("text"), Some(&JsValue::from("before")));
        assert!(report.state.tool_calls.is_empty());
        assert!(!report.state.has_message_end);
        assert_eq!(
            h.into_exchange()
                .calls
                .iter()
                .map(|(_, op, _, _)| *op)
                .collect::<Vec<_>>(),
            ["stream_next", "close_stream"]
        );
    }
}

// Fixed original hostModelInvokerPort.ts: successful stream_next admits only
// events:Array + done:boolean and forwards each event. Independent packet
// adapter-prior-fixture-fresh-execution-stream-unknown-finish actually observed
// custom finishReason unchanged; all additional cases below are source-derived.
#[test]
fn open_finish_reply_reaches_source_invocation_and_retains_live_boundary() {
    use pilotdeck_agent::driver::InvocationDisposition;
    use pilotdeck_agent::source_invocation::run_source_model_invocation;
    let raw = json(
        r#"[{"type":"text_delta","text":"known"},{"type":"message_end","finishReason":"future_finish","extension":"\ud800"}]"#,
    );
    let mut h = host(vec![
        response(JsValue::Object(object(&[
            ("events", raw.clone()),
            ("done", JsValue::Bool(true)),
        ]))),
        response(JsValue::Null),
    ]);
    let mut live = Vec::new();
    let report = run_source_model_invocation(
        &mut h,
        &control(),
        &control(),
        &invocation_prepared(),
        &hints(),
        "response".into(),
        &mut |e| {
            live.push(e);
            Ok(())
        },
    );
    assert!(matches!(
        report.disposition,
        InvocationDisposition::Assembled
    ));
    let assembled = report.assembled.unwrap();
    assert_eq!(
        assembled.value().get("finishReason"),
        Some(&JsValue::from("future_finish"))
    );
    assert_eq!(
        assembled.value().get("hasMessageEnd"),
        Some(&JsValue::Bool(true))
    );
    let JsValue::Array(events) = &raw else {
        panic!()
    };
    assert_eq!(live.len(), 2);
    for (actual, expected) in live.iter().zip(events) {
        assert_eq!(actual.boundary.value(), expected);
    }
    assert_eq!(
        h.into_exchange()
            .calls
            .iter()
            .map(|(_, op, _, _)| *op)
            .collect::<Vec<_>>(),
        ["stream_next"]
    );
}
#[test]
fn known_unprojectable_fields_are_successful_raw_stream_boundaries_not_unknown_effects() {
    let raw = json(
        r#"[{"type":"request_started","model":{"open":"model"},"metadata":null},{"type":"message_end","finishReason":null},{"type":"usage","usage":{"inputTokens":"2"}},{"type":"tool_call_end","toolCall":{"id":"one","extension":null}},{"type":"thinking_delta","text":"think","signature":["signature"]},{"type":"thinking_delta","text":"prefix","reasoningContent":{"toString":null}},{"type":"thinking_delta","text":"prefix","signature":null}]"#,
    );
    let mut h = host(vec![response(JsValue::Object(object(&[
        ("events", raw.clone()),
        ("done", JsValue::Bool(true)),
    ])))]);
    let HostReply::ModelChunk(chunk) = h
        .call(
            &control(),
            HostCall::ModelPull {
                handle: handle(),
                request: Box::new(request()),
                hints: hints(),
            },
        )
        .unwrap()
    else {
        panic!()
    };
    let JsValue::Array(events) = &raw else {
        panic!()
    };
    assert_eq!(chunk.events.len(), events.len());
    for (actual, expected) in chunk.events.iter().zip(events) {
        let StreamEvent::BoundaryOnly(boundary) = actual else {
            panic!("normalized limitation must retain whole source boundary: {actual:?}")
        };
        assert_eq!(boundary.value(), expected);
    }
    assert!(chunk.done);
    assert_eq!(h.into_exchange().calls.len(), 1);
}
#[test]
fn open_fields_and_lax_structured_call_reduce_without_fabricated_tool_defaults() {
    use pilotdeck_agent::driver::InvocationDisposition;
    use pilotdeck_agent::source_invocation::run_source_model_invocation;
    let raw = json(
        r#"[{"type":"request_started","model":{"open":"model"}},{"type":"usage","usage":{"inputTokens":"2"}},{"type":"thinking_delta","text":"think","signature":["sig"]},{"type":"tool_call_end","toolCall":{"id":"one","extension":null}},{"type":"message_end","finishReason":"future_finish"}]"#,
    );
    let mut h = host(vec![response(JsValue::Object(object(&[
        ("events", raw.clone()),
        ("done", JsValue::Bool(true)),
    ])))]);
    let mut live = Vec::new();
    let report = run_source_model_invocation(
        &mut h,
        &control(),
        &control(),
        &invocation_prepared(),
        &hints(),
        "response".into(),
        &mut |e| {
            live.push(e);
            Ok(())
        },
    );
    assert!(matches!(
        report.disposition,
        InvocationDisposition::Assembled
    ));
    assert!(report.state.has_message_end);
    let assembled = report.assembled.unwrap();
    assert_eq!(
        assembled.value().get("finishReason"),
        Some(&JsValue::from("future_finish"))
    );
    assert_eq!(
        assembled.value().get("usage").unwrap().get("inputTokens"),
        Some(&JsValue::from("02"))
    );
    let JsValue::Array(calls) = assembled.value().get("toolCalls").unwrap() else {
        panic!()
    };
    assert_eq!(calls, &vec![json(r#"{"id":"one","extension":null}"#)]);
    assert!(calls[0].get("name").is_none());
    assert!(calls[0].get("input").is_none());
    let message = assembled.value().get("message").unwrap();
    assert_eq!(
        message.get("metadata").unwrap().get("model"),
        Some(&json(r#"{"open":"model"}"#))
    );
    let JsValue::Array(content) = message.get("content").unwrap() else {
        panic!()
    };
    assert_eq!(content[0].get("signature"), Some(&json(r#"["sig"]"#)));
    assert_eq!(live.len(), 5);
    assert_eq!(h.into_exchange().calls.len(), 1);
}
#[test]
fn consumer_signature_type_error_is_not_transport_unknown_and_preserves_partial_state() {
    use pilotdeck_agent::driver::InvocationDisposition;
    use pilotdeck_agent::source_invocation::run_source_model_invocation;
    let delta = json(
        r#"{"type":"thinking_delta","text":"prefix","reasoningContent":"reason","signature":null}"#,
    );
    let raw = JsValue::Array(vec![
        delta.clone(),
        json(r#"{"type":"tool_call_end","toolCall":{"id":"forbidden","name":"read","input":{}}}"#),
    ]);
    let mut h = host(vec![
        response(JsValue::Object(object(&[
            ("events", raw),
            ("done", JsValue::Bool(true)),
        ]))),
        response(JsValue::Null),
    ]);
    let mut live = Vec::new();
    let report = run_source_model_invocation(
        &mut h,
        &control(),
        &control(),
        &invocation_prepared(),
        &hints(),
        "response".into(),
        &mut |e| {
            live.push(e);
            Ok(())
        },
    );
    assert!(
        matches!(&report.disposition,InvocationDisposition::PortFailure(error) if matches!(error.as_ref(),PortError::SourceException {message,boundary} if message.equals_str("Cannot read properties of null (reading 'length')") && boundary.value()==&delta))
    );
    assert_eq!(live.len(), 1);
    assert_eq!(live[0].boundary.value(), &delta);
    assert!(!report.state.has_message_end);
    assert!(report.state.tool_calls.is_empty());
    // assemble flushes already applied text/reasoning despite the later getter exception.
    let assembled = report.assembled.unwrap();
    let JsValue::Array(content) = assembled
        .value()
        .get("message")
        .unwrap()
        .get("content")
        .unwrap()
    else {
        panic!()
    };
    assert_eq!(content[0].get("text"), Some(&JsValue::from("prefix")));
    assert_eq!(
        content[0].get("reasoningContent"),
        Some(&JsValue::from("reason"))
    );
    assert_eq!(
        h.into_exchange()
            .calls
            .iter()
            .map(|(_, op, _, _)| *op)
            .collect::<Vec<_>>(),
        ["stream_next", "close_stream"]
    );
}
#[test]
fn unprojectable_events_do_not_relax_outer_reply_or_accepted_identity_checks() {
    for p in [
        json(r#"{"events":{},"done":true}"#),
        json(r#"{"events":[{"type":"message_end","finishReason":"future"}],"done":0}"#),
        json(r#"{"events":[{"type":"usage","usage":null}]}"#),
    ] {
        let mut h = host(vec![response(p)]);
        let error = h
            .call(
                &control(),
                HostCall::ModelPull {
                    handle: handle(),
                    request: Box::new(request()),
                    hints: hints(),
                },
            )
            .unwrap_err();
        assert!(matches!(error,PortError::AcceptedUnknown {identity,..} if *identity==owner()));
        assert_eq!(h.into_exchange().calls.len(), 1);
    }
}

#[test]
fn eager_stream_open_known_boundary_is_forwarded_without_a_pull_done_requirement() {
    let event =
        json(r#"{"type":"message_end","finishReason":"future_finish","raw":{"keep":null}}"#);
    let mut h = host(vec![response(JsValue::Object(object(&[(
        "events",
        JsValue::Array(vec![event.clone()]),
    )])))]);
    let HostReply::ModelEagerEvents(events) = h
        .call(
            &control(),
            HostCall::ModelEagerStream {
                handle: handle(),
                request: Some(Box::new(request())),
                hints: hints(),
            },
        )
        .unwrap()
    else {
        panic!()
    };
    assert_eq!(events.len(), 1);
    assert!(matches!(&events[0],StreamEvent::BoundaryOnly(boundary) if boundary.value()==&event));
    assert_eq!(
        h.into_exchange()
            .calls
            .iter()
            .map(|(_, op, _, _)| *op)
            .collect::<Vec<_>>(),
        ["stream"]
    );
}

#[test]
fn pure_source_assembly_projection_keeps_open_finish_and_exact_source_boundary() {
    use pilotdeck_agent::source_model::{OriginalSourceModelReducer, SourceModelReducer};
    use pilotdeck_protocol::model::FinishReason;
    let reducer = OriginalSourceModelReducer;
    let mut state = reducer.create(&JsValue::from("response"));
    for raw in [
        r#"{"type":"request_started","provider":"p","model":"m"}"#,
        r#"{"type":"text_delta","text":"before"}"#,
        r#"{"type":"tool_call_end","toolCall":{"id":"one","name":"read","input":{},"extension":null}}"#,
        r#"{"type":"usage","usage":{"inputTokens":2,"nativeCost":0}}"#,
        r#"{"type":"message_end","finishReason":"future_finish"}"#,
    ] {
        reducer
            .apply(&mut state, &BoundarySnapshot::capture(&json(raw)))
            .unwrap();
    }
    let raw = reducer.assemble(&mut state).unwrap();
    let projected = admit_assistant_assembly(raw.value()).unwrap();
    assert_eq!(projected.boundary.value(), raw.value());
    assert_eq!(projected.finish_reason, FinishReason::Unknown);
    assert_eq!(
        projected.boundary.value().get("finishReason"),
        Some(&JsValue::from("future_finish"))
    );
    assert!(projected.has_message_end);
    assert!(projected.error.is_none());
    assert_eq!(projected.usage.unwrap().native_cost, Some(0.0));
    assert_eq!(projected.tool_calls.len(), 1);
    assert_eq!(projected.tool_calls[0].id, JsString::from("one"));
    assert!(
        matches!(&projected.message.content[0].body,ContentBody::Text(t) if t.text.equals_str("before"))
    );
    assert!(matches!(raw.value().get("error"), Some(JsValue::Undefined)));
}
#[test]
fn pure_assembly_projection_keeps_canonical_error_and_repaired_presence() {
    use pilotdeck_agent::source_model::{OriginalSourceModelReducer, SourceModelReducer};
    use pilotdeck_protocol::model::FinishReason;
    let reducer = OriginalSourceModelReducer;
    let mut state = reducer.create(&JsValue::from("response"));
    for raw in [
        r#"{"type":"tool_call_end","toolCall":{"id":"one","name":"read","input":{}},"wasRepaired":true}"#,
        r#"{"type":"error","error":{"provider":"p","protocol":"openai","code":"limit","message":"bad","retryable":true,"raw":{"extension":null}}}"#,
    ] {
        reducer
            .apply(&mut state, &BoundarySnapshot::capture(&json(raw)))
            .unwrap();
    }
    let raw = reducer.assemble(&mut state).unwrap();
    let projected = admit_assistant_assembly(raw.value()).unwrap();
    assert_eq!(projected.boundary.value(), raw.value());
    assert_eq!(projected.finish_reason, FinishReason::Error);
    assert!(!projected.has_message_end);
    assert_eq!(projected.has_repaired_tool_calls, Some(true));
    assert_eq!(projected.error.unwrap().code, JsString::from("limit"));
    assert!(projected.usage.is_none());
}
#[test]
fn pure_projection_gap_is_explicit_and_never_fabricates_empty_normal_message_or_unknown_effect() {
    let raw = json(
        r#"{"message":{"role":"assistant","content":[{"type":"text"}]},"finishReason":"stop","hasMessageEnd":true,"toolCalls":[]}"#,
    );
    let error = admit_assistant_assembly(&raw).unwrap_err();
    assert!(
        matches!(error,PortError::InvalidResponse {message} if message.equals_str("Unimplemented normal-turn projection: assembled message is BoundaryOnly"))
    );
    for raw in [
        r#"{"message":{"role":"assistant","content":[]},"finishReason":"stop","hasMessageEnd":0,"toolCalls":[]}"#,
        r#"{"message":{"role":"assistant","content":[]},"finishReason":"stop","hasMessageEnd":true,"toolCalls":[{"id":"one"}]}"#,
        r#"{"message":{"role":"assistant","content":[]},"finishReason":"stop","hasMessageEnd":true,"toolCalls":[],"usage":{"inputTokens":"2"}}"#,
        r#"{"message":{"role":"assistant","content":[]},"finishReason":"stop","hasMessageEnd":true,"toolCalls":[],"error":null}"#,
        r#"{"message":{"role":"assistant","content":[]},"finishReason":7,"hasMessageEnd":true,"toolCalls":[]}"#,
    ] {
        assert!(matches!(
            admit_assistant_assembly(&json(raw)),
            Err(PortError::InvalidResponse { .. })
        ));
    }
}

#[test]
fn usage_null_is_a_later_source_getter_exception_after_live_prefix_not_transport_uncertainty() {
    use pilotdeck_agent::driver::InvocationDisposition;
    use pilotdeck_agent::source_invocation::run_source_model_invocation;
    let usage = json(r#"{"type":"usage","usage":null}"#);
    let raw = JsValue::Array(vec![
        json(r#"{"type":"text_delta","text":"prefix"}"#),
        usage.clone(),
        json(r#"{"type":"message_end","finishReason":"stop"}"#),
    ]);
    let mut h = host(vec![
        response(JsValue::Object(object(&[
            ("events", raw),
            ("done", JsValue::Bool(true)),
        ]))),
        response(JsValue::Null),
    ]);
    let mut live = Vec::new();
    let report = run_source_model_invocation(
        &mut h,
        &control(),
        &control(),
        &invocation_prepared(),
        &hints(),
        "response".into(),
        &mut |e| {
            live.push(e);
            Ok(())
        },
    );
    assert!(
        matches!(&report.disposition,InvocationDisposition::PortFailure(error) if matches!(error.as_ref(),PortError::SourceException {message,boundary} if message.equals_str("Cannot read properties of null (reading 'inputTokens')") && boundary.value()==&usage))
    );
    assert_eq!(live.len(), 2);
    assert_eq!(live[1].boundary.value(), &usage);
    assert!(!report.state.has_message_end);
    assert!(report.state.tool_calls.is_empty());
    let assembled = report.assembled.unwrap();
    let JsValue::Array(content) = assembled
        .value()
        .get("message")
        .unwrap()
        .get("content")
        .unwrap()
    else {
        panic!()
    };
    assert_eq!(content[0].get("text"), Some(&JsValue::from("prefix")));
    assert_eq!(
        h.into_exchange()
            .calls
            .iter()
            .map(|(_, op, _, _)| *op)
            .collect::<Vec<_>>(),
        ["stream_next", "close_stream"]
    );
}

#[test]
fn ack_first_failure_only_rechecks_abort_when_the_stdio_consumer_throws() {
    struct ReplyThenCancel {
        raw: JsValue,
        cause: CancelCause,
    }
    impl HostExchange for ReplyThenCancel {
        fn exchange(
            &mut self,
            control: &CallControl,
            _: &'static str,
            _: &'static str,
            _: CallEffect,
            _: JsObject,
        ) -> PortResult<HostExchangeResponse> {
            let response = HostExchangeResponse {
                identity: owner(),
                boundary: BoundarySnapshot::capture(&self.raw),
                cancelled_before_response: false,
            };
            assert!(control.cancellation.cancel(self.cause.clone()));
            Ok(response)
        }
    }
    for bad_message in [false, true] {
        for cause in [
            CancelCause::Requested("stop".into()),
            CancelCause::OwnerRetired,
            CancelCause::TransportClosed,
        ] {
            let message = if bad_message {
                json(r#"{"toString":null}"#)
            } else {
                "Known failure".into()
            };
            let raw = JsValue::Object(object(&[
                ("kind", "response".into()),
                ("messageId", "m".into()),
                ("inReplyTo", "reply".into()),
                ("ok", JsValue::Bool(false)),
                ("error", JsValue::Object(object(&[("message", message)]))),
            ]));
            let mut h = RemoteHost::new(
                ReplyThenCancel {
                    raw,
                    cause: cause.clone(),
                },
                AdvertisedMethods {
                    modules: vec![("capability".into(), vec!["execute_batch".into()])],
                },
            );
            let result = h.call(
                &control(),
                HostCall::ExecuteBatch {
                    calls: vec![invocation("a")],
                    hints: tools(),
                },
            );
            if matches!(
                cause,
                CancelCause::OwnerRetired | CancelCause::TransportClosed
            ) {
                assert!(
                    matches!(result, Err(PortError::Cancelled { cause: actual }) if actual == cause)
                );
            } else if bad_message {
                assert!(
                    matches!(result, Ok(HostReply::ToolBatchRejected { message, identity, .. }) if message.equals_str("Tool execution cancelled.") && *identity == owner())
                );
            } else {
                assert!(
                    matches!(result, Ok(HostReply::ToolResults(results)) if results.len() == 1)
                );
            }
        }
    }
}
