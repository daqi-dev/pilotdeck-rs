use super::*;
use crate::{
    config::permission::PermissionRule,
    host::modules::permission::{PermissionContext as _, PermissionDecision as _},
    runtime::project::{InteractionProfileName, RuntimeProfile, RuntimeProfileOverrides},
};
struct Catalog;
impl ToolCatalog for Catalog {
    fn list(&self) -> HostFuture<Vec<Arc<dyn ToolDefinition>>> {
        Box::pin(async { Ok(vec![tool()]) })
    }
}
fn profile(name: InteractionProfileName) -> InteractionProfile {
    RuntimeProfile::resolve(
        &JsValue::Object(JsObject::new()),
        &RuntimeProfileOverrides {
            interaction_profile_override: Some(name),
            ..Default::default()
        },
    )
    .interaction
}
fn bind(
    resource: &InteractionPermission,
    name: InteractionProfileName,
    session: &str,
    coordinator: Arc<GatewayInteractionCoordinator>,
    emit: Arc<dyn Fn(JsValue) -> bool + Send + Sync>,
) -> (TurnPermission, ModuleRouter) {
    let mut router = ModuleRouter::default();
    let options = PermissionTurnOptions {
        profile: profile(name),
        context: permission::create_default_permission_context(
            permission::DefaultPermissionContextOptions {
                cwd: "/workspace".into(),
                can_prompt: Some(true),
                ..Default::default()
            },
        ),
        rules: RuleSetHandle::empty(),
        coordinator,
        emit,
        timeout: Duration::from_millis(5),
    };
    let turn = resource
        .bind_turn(
            session.into(),
            "same-run".into(),
            options,
            Arc::new(Catalog),
            &mut router,
        )
        .unwrap();
    (turn, router)
}
fn tool() -> Arc<dyn ToolDefinition> {
    Arc::new(crate::runtime::tools::builtin::bash::BashTool::new())
}
struct NamedTool(JsObject);
impl ToolDefinition for NamedTool {
    fn fields(&self) -> &JsObject {
        &self.0
    }
}
fn named_tool(name: &str) -> Arc<dyn ToolDefinition> {
    let mut fields = JsObject::new();
    fields.insert("name".into(), name.into());
    Arc::new(NamedTool(fields))
}
fn behavior(value: &JsValue, field: &str, expected: &str) {
    assert!(
        value
            .get(field)
            .and_then(JsValue::as_string)
            .is_some_and(|v| v.equals_str(expected)),
        "{value:?}"
    );
}
#[tokio::test]
async fn profiles_decide_from_trusted_context_and_live_rules() {
    for name in [
        InteractionProfileName::Interactive,
        InteractionProfileName::Headless,
        InteractionProfileName::Disabled,
    ] {
        let resource = InteractionPermission::default();
        let (turn, _) = bind(
            &resource,
            name,
            "s",
            Arc::default(),
            Arc::new(|_| panic!("decision must not prompt")),
        );
        let context = turn
            .prepare(object([("mode", "bypassPermissions".into())]))
            .await
            .unwrap();
        let result = turn
            .decide(
                tool(),
                object([("command", "pwd".into())]),
                context.clone(),
                "call".into(),
            )
            .await
            .unwrap();
        behavior(&result, "type", "allow");
        turn.run.options.rules.update_member("deny", |rules| {
            rules.push(PermissionRule::new("project", "deny", "bash").into())
        });
        let result = turn
            .decide(tool(), JsValue::Null, context, "call".into())
            .await
            .unwrap();
        behavior(&result, "type", "deny");
    }
}

#[tokio::test]
async fn profiled_decisions_match_ts_bundle_including_native_denials() {
    let JsValue::Array(ref rows) = pilotdeck_protocol::parse_json(include_str!(
        "../../../../tests/fixtures/interaction-permission/profiles.json"
    ))
    .unwrap() else {
        panic!("profile oracle must be an array")
    };
    for row in rows {
        let name = match row
            .get("name")
            .unwrap()
            .as_string()
            .unwrap()
            .to_utf8()
            .unwrap()
            .as_str()
        {
            "interactive" => InteractionProfileName::Interactive,
            "headless" => InteractionProfileName::Headless,
            "disabled" => InteractionProfileName::Disabled,
            _ => panic!("unknown profile"),
        };
        let coordinator = Arc::new(GatewayInteractionCoordinator::default());
        let rules = RuleSetHandle::empty();
        let forced = row.get("forcedMcp").unwrap().as_bool().unwrap();
        let tool_name = if forced { "mcp__danger" } else { "danger" };
        if forced {
            rules.update_member("ask", |rules| {
                rules.push(
                    PermissionRule::new("session", "ask", tool_name)
                        .with_force(true)
                        .into(),
                );
            });
        }
        let options = PermissionTurnOptions {
            profile: profile(name),
            context: permission::create_default_permission_context(
                permission::DefaultPermissionContextOptions {
                    cwd: "/workspace".into(),
                    can_prompt: row.get("canPrompt").and_then(JsValue::as_bool),
                    policy_can_prompt: row.get("policyCanPrompt").and_then(JsValue::as_bool),
                    ..Default::default()
                },
            ),
            rules,
            coordinator: coordinator.clone(),
            emit: Arc::new(|_| panic!("decide must not create a prompt")),
            timeout: Duration::ZERO,
        };
        let resource = InteractionPermission::default();
        let turn = resource
            .bind_turn(
                "s".into(),
                "r".into(),
                options,
                Arc::new(Catalog),
                &mut ModuleRouter::default(),
            )
            .unwrap();
        let context = turn.prepare(JsValue::Null).await.unwrap();
        let decision = turn
            .decide(named_tool(tool_name), JsValue::Null, context, "call".into())
            .await
            .unwrap();
        assert_eq!(&decision, row.get("result").unwrap(), "{row:?}");
        assert_eq!(row.get("emitted"), Some(&JsValue::Number(0.0)));
        assert_eq!(coordinator.permission_bus().pending_count(&"s".into()), 0);
        assert!(turn.run.options.rules.snapshot().allow.is_empty());
        // Forced MCP ask reaches the profile gate despite context.canPrompt=false.
        // Direct callback misuse must also fail closed with the same short message.
        if forced {
            let response = prompt(&turn, Cancellation::new()).await.unwrap();
            assert_eq!(response.get("message"), decision.get("message"));
            behavior(&response, "behavior", "deny");
        }
    }
}

#[tokio::test]
async fn gateway_policy_denial_precedes_profile_admission_for_direct_requests() {
    // TS has no request(context) port. Its effective bundle.decide path keeps
    // this native policy denial for both headless and disabled profiles.
    for name in [
        InteractionProfileName::Interactive,
        InteractionProfileName::Headless,
        InteractionProfileName::Disabled,
    ] {
        for can_prompt in [true, false] {
            let coordinator = Arc::new(GatewayInteractionCoordinator::default());
            let resource = InteractionPermission::default();
            let turn = resource
                .bind_turn(
                    "s".into(),
                    "r".into(),
                    PermissionTurnOptions {
                        profile: profile(name),
                        context: permission::create_default_permission_context(
                            permission::DefaultPermissionContextOptions {
                                cwd: "/workspace".into(),
                                can_prompt: Some(can_prompt),
                                policy_can_prompt: Some(false),
                                ..Default::default()
                            },
                        ),
                        rules: RuleSetHandle::empty(),
                        coordinator: coordinator.clone(),
                        emit: Arc::new(|_| panic!("policy denial must not emit")),
                        timeout: Duration::ZERO,
                    },
                    Arc::new(Catalog),
                    &mut ModuleRouter::default(),
                )
                .unwrap();
            let message: JsValue =
                "Permission prompt denied because Gateway policy disables prompts for this session."
                    .into();
            let context = turn.prepare(JsValue::Null).await.unwrap();
            assert_eq!(
                turn.decide(named_tool("danger"), JsValue::Null, context, "call".into())
                    .await
                    .unwrap(),
                object([
                    ("type", "deny".into()),
                    (
                        "reason",
                        object([("type", "runtime".into()), ("message", message.clone())]),
                    ),
                    ("message", message.clone()),
                ]),
            );
            assert_eq!(
                prompt(&turn, Cancellation::new()).await.unwrap(),
                object([("behavior", "deny".into()), ("message", message)]),
                "{name:?}, can_prompt={can_prompt}",
            );
            assert_eq!(coordinator.permission_bus().pending_count(&"s".into()), 0);
            assert!(lock(&turn.run.requests).is_empty());
            assert!(turn.run.options.rules.snapshot().allow.is_empty());
        }
    }
}

#[tokio::test]
async fn emit_panic_unwinds_but_drops_the_pending_registration() {
    // Fn -> bool has no error channel. This records the Rust boundary, not a
    // claim of parity with TS's catch(error) -> delivery-failed deny branch.
    let coordinator = Arc::new(GatewayInteractionCoordinator::default());
    let resource = InteractionPermission::default();
    let (turn, _) = bind(
        &resource,
        InteractionProfileName::Interactive,
        "s",
        coordinator.clone(),
        Arc::new(|_| panic!("sink failed")),
    );
    let error = tokio::spawn(prompt(&turn, Cancellation::new()))
        .await
        .unwrap_err();
    assert!(error.is_panic());
    assert_eq!(coordinator.permission_bus().pending_count(&"s".into()), 0);
    assert!(lock(&turn.run.requests).is_empty());
    assert!(turn.run.options.rules.snapshot().allow.is_empty());
    assert!(turn.prepare(JsValue::Null).await.is_ok());
}

#[tokio::test]
async fn synchronous_answer_wins_over_delivery_false() {
    let coordinator = Arc::new(GatewayInteractionCoordinator::default());
    let answerer = coordinator.clone();
    let resource = InteractionPermission::default();
    let (turn, _) = bind(
        &resource,
        InteractionProfileName::Interactive,
        "s",
        coordinator.clone(),
        Arc::new(move |event| {
            assert!(answerer.decide_permission(
                &"s".into(),
                crate::runtime::interaction::PermissionDecision {
                    request_id: event.get("requestId").unwrap().as_string().unwrap().clone(),
                    decision: "allow".into(),
                    remember: Some(JsValue::Bool(true)),
                    reason: None,
                },
                None,
            ));
            false
        }),
    );
    let JsValue::Array(ref hooks) = pilotdeck_protocol::parse_json(include_str!(
        "../../../../tests/fixtures/interaction-permission/hooks.json"
    ))
    .unwrap() else {
        panic!("hook oracle must be an array")
    };
    let expected = hooks
        .iter()
        .find(|row| {
            row.get("mode")
                .and_then(JsValue::as_string)
                .is_some_and(|mode| mode.equals_str("answer-then-no-sink"))
        })
        .unwrap();
    assert_eq!(
        prompt(&turn, Cancellation::new()).await.unwrap(),
        *expected
            .get("result")
            .unwrap()
            .get("specific")
            .unwrap()
            .get("decision")
            .unwrap(),
    );
    let rules = turn.run.options.rules.snapshot().allow;
    assert_eq!(
        JsValue::Array(rules.iter().map(PermissionRule::to_value).collect()),
        *expected.get("rules").unwrap(),
    );
    assert_eq!(coordinator.permission_bus().pending_count(&"s".into()), 0);
}

#[tokio::test]
async fn profiles_prompt_fail_closed_and_interactive_roundtrip_remembers() {
    for name in [
        InteractionProfileName::Interactive,
        InteractionProfileName::Headless,
        InteractionProfileName::Disabled,
    ] {
        let coordinator = Arc::new(GatewayInteractionCoordinator::default());
        let answerer = coordinator.clone();
        let emit = Arc::new(move |event: JsValue| {
            assert_eq!(name, InteractionProfileName::Interactive);
            assert!(!answerer.decide_permission(
                &"other".into(),
                crate::runtime::interaction::PermissionDecision {
                    request_id: event.get("requestId").unwrap().as_string().unwrap().clone(),
                    decision: "allow".into(),
                    remember: None,
                    reason: None
                },
                None
            ));
            answerer.decide_permission(
                &"s".into(),
                crate::runtime::interaction::PermissionDecision {
                    request_id: event.get("requestId").unwrap().as_string().unwrap().clone(),
                    decision: "allow".into(),
                    remember: Some(JsValue::Bool(true)),
                    reason: None,
                },
                None,
            )
        });
        let resource = InteractionPermission::default();
        let (turn, _) = bind(&resource, name, "s", coordinator.clone(), emit);
        let context = turn.prepare(JsValue::Null).await.unwrap();
        let before = turn
            .decide(
                named_tool("danger"),
                JsValue::Null,
                context.clone(),
                "call".into(),
            )
            .await
            .unwrap();
        behavior(
            &before,
            "type",
            if name == InteractionProfileName::Interactive {
                "ask"
            } else {
                "deny"
            },
        );
        let result = prompt(&turn, Cancellation::new()).await.unwrap();
        behavior(
            &result,
            "behavior",
            if name == InteractionProfileName::Interactive {
                "allow"
            } else {
                "deny"
            },
        );
        if name == InteractionProfileName::Interactive {
            assert_eq!(result, oracle_decision(0),);
            let rules = turn.run.options.rules.snapshot().allow;
            assert_eq!(
                rules,
                vec![PermissionRule::new("session", "allow", "danger")]
            );
            assert_eq!(
                JsValue::Array(rules.iter().map(PermissionRule::to_value).collect()),
                *oracle_row(0).get("rules").unwrap(),
            );
            let after = turn
                .decide(
                    named_tool("danger"),
                    JsValue::Null,
                    context,
                    "next-call".into(),
                )
                .await
                .unwrap();
            behavior(&after, "type", "allow");
            assert_eq!(
                after.get("reason").unwrap().get("rule"),
                Some(&rules[0].to_value()),
            );
        } else {
            assert_eq!(result.get("message"), before.get("message"));
            assert!(turn.run.options.rules.snapshot().allow.is_empty());
        }
        assert_eq!(coordinator.permission_bus().pending_count(&"s".into()), 0);
    }
}
#[tokio::test]
async fn waits_cancel_drop_timeout_delivery_and_generation_close() {
    let coordinator = Arc::new(GatewayInteractionCoordinator::default());
    let resource = InteractionPermission::default();
    let (turn, _) = bind(
        &resource,
        InteractionProfileName::Interactive,
        "s",
        coordinator.clone(),
        Arc::new(|_| true),
    );
    let cancel = Cancellation::new();
    let mut request = prompt(&turn, cancel.clone());
    park(&mut request).await;
    assert_eq!(coordinator.permission_bus().pending_count(&"s".into()), 1);
    cancel.cancel();
    behavior(&request.await.unwrap(), "behavior", "deny");
    let mut request = prompt(&turn, Cancellation::new());
    park(&mut request).await;
    drop(request);
    assert_eq!(coordinator.permission_bus().pending_count(&"s".into()), 0);
    behavior(
        &prompt(&turn, Cancellation::new()).await.unwrap(),
        "behavior",
        "deny",
    );
    let mut request = prompt(&turn, Cancellation::new());
    park(&mut request).await;
    resource.close();
    resource.close();
    assert!(request.await.is_err());
    assert_eq!(resource.release_count(), 1);
    assert!(resource.is_closed());
    assert_eq!(coordinator.permission_bus().pending_count(&"s".into()), 0);
    assert!(turn.prepare(JsValue::Null).await.is_err());
    let other = InteractionPermission::default();
    let (no_sink, _) = bind(
        &other,
        InteractionProfileName::Interactive,
        "s",
        coordinator,
        Arc::new(|_| false),
    );
    behavior(
        &prompt(&no_sink, Cancellation::new()).await.unwrap(),
        "behavior",
        "deny",
    );
}
#[tokio::test]
async fn superseded_run_cannot_answer_or_revoke_new_run_and_generations_are_isolated() {
    let coordinator = Arc::new(GatewayInteractionCoordinator::default());
    let ids = Arc::new(Mutex::new(Vec::<JsString>::new()));
    let recorded = ids.clone();
    let emit: Arc<dyn Fn(JsValue) -> bool + Send + Sync> = Arc::new(move |event| {
        lock(&recorded).push(event.get("requestId").unwrap().as_string().unwrap().clone());
        true
    });
    let resource = InteractionPermission::default();
    let (old, _) = bind(
        &resource,
        InteractionProfileName::Interactive,
        "s",
        coordinator.clone(),
        emit.clone(),
    );
    let mut pending = prompt(&old, Cancellation::new());
    park(&mut pending).await;
    let (next, _) = bind(
        &resource,
        InteractionProfileName::Interactive,
        "s",
        coordinator.clone(),
        emit.clone(),
    );
    assert!(pending.await.is_err());
    old.close();
    assert!(next.prepare(JsValue::Null).await.is_ok());
    let new_generation = InteractionPermission::default();
    let (new, _) = bind(
        &new_generation,
        InteractionProfileName::Interactive,
        "s",
        coordinator.clone(),
        emit,
    );
    let context = new.prepare(JsValue::Null).await.unwrap();
    assert!(
        next.decide(tool(), JsValue::Null, context, "call".into())
            .await
            .is_err()
    );
    resource.close();
    assert!(new.prepare(JsValue::Null).await.is_ok());
    assert!(!coordinator.decide_permission(
        &"s".into(),
        crate::runtime::interaction::PermissionDecision {
            request_id: lock(&ids)[0].clone(),
            decision: "allow".into(),
            remember: None,
            reason: None
        },
        None
    ));
}

fn prompt(turn: &TurnPermission, cancel: Cancellation) -> HostFuture<JsValue> {
    turn.request(
        "danger".into(),
        "call".into(),
        JsValue::Null,
        vec![],
        cancel,
    )
}
async fn park(request: &mut HostFuture<JsValue>) {
    assert!(
        std::future::poll_fn(|cx| std::task::Poll::Ready(request.as_mut().poll(cx).is_pending()))
            .await
    );
}

fn oracle_row(index: usize) -> JsValue {
    let golden = pilotdeck_protocol::parse_json(include_str!(
        "../../../../tests/fixtures/interaction-permission/oracle.json"
    ))
    .unwrap();
    let JsValue::Array(ref rows) = golden else {
        panic!("array oracle")
    };
    rows[index].clone()
}
fn oracle_decision(index: usize) -> JsValue {
    oracle_row(index)
        .get("result")
        .unwrap()
        .get("specific")
        .unwrap()
        .get("decision")
        .unwrap()
        .clone()
}
#[tokio::test]
async fn oracle_no_sink_and_pre_cancel_match_entire_decision() {
    for (index, cancelled) in [(1, false), (2, true)] {
        let coordinator = Arc::new(GatewayInteractionCoordinator::default());
        let resource = InteractionPermission::default();
        let (turn, _) = bind(
            &resource,
            InteractionProfileName::Interactive,
            "s",
            coordinator.clone(),
            Arc::new(move |_| {
                assert!(!cancelled, "pre-cancel must not register or emit");
                false
            }),
        );
        let cancel = Cancellation::new();
        if cancelled {
            cancel.cancel();
        }
        assert_eq!(prompt(&turn, cancel).await.unwrap(), oracle_decision(index));
        assert_eq!(coordinator.permission_bus().pending_count(&"s".into()), 0);
        let rules = turn.run.options.rules.snapshot().allow;
        assert_eq!(
            JsValue::Array(rules.iter().map(PermissionRule::to_value).collect()),
            *oracle_row(index).get("rules").unwrap(),
        );
    }
}
#[tokio::test]
async fn deny_nullish_reason_uses_ts_nullish_fallback() {
    for reason in [
        None,
        Some(JsValue::Null),
        Some("specific reason".into()),
        Some("".into()),
    ] {
        let coordinator = Arc::new(GatewayInteractionCoordinator::default());
        let answerer = coordinator.clone();
        let expected = reason
            .clone()
            .filter(|v| !matches!(v, JsValue::Null))
            .unwrap_or("Permission prompt denied.".into());
        let resource = InteractionPermission::default();
        let (turn, _) = bind(
            &resource,
            InteractionProfileName::Interactive,
            "s",
            coordinator,
            Arc::new(move |event| {
                answerer.decide_permission(
                    &"s".into(),
                    crate::runtime::interaction::PermissionDecision {
                        request_id: event.get("requestId").unwrap().as_string().unwrap().clone(),
                        decision: "deny".into(),
                        remember: None,
                        reason: reason.clone(),
                    },
                    None,
                )
            }),
        );
        assert_eq!(
            prompt(&turn, Cancellation::new()).await.unwrap(),
            object([("behavior", "deny".into()), ("message", expected)])
        );
    }
}
#[tokio::test]
async fn production_catalog_keeps_native_permission_checks() {
    use crate::runtime::model::http::HyperModelTransport;
    use crate::runtime::tools::{
        builtin::web::{
            UrlFetcher, WebFetchTool, WebFetchToolOptions, WebSearchOptions, WebSearchTool,
        },
        registry::ToolRegistry,
        scheduler::{SchedulerMode, ToolRuntime},
    };
    struct MissingPermissionForwarder(Arc<dyn ToolDefinition>);
    impl ToolDefinition for MissingPermissionForwarder {
        fn fields(&self) -> &JsObject {
            self.0.fields()
        }
        fn is_read_only(&self, input: &JsValue) -> Result<JsValue, HostError> {
            self.0.is_read_only(input)
        }
    }
    let resource = InteractionPermission::default();
    let (turn, _) = bind(
        &resource,
        InteractionProfileName::Interactive,
        "s",
        Arc::default(),
        Arc::new(|_| false),
    );
    let registry = ToolRegistry::default();
    let _registration = registry
        .register(Arc::new(
            crate::runtime::tools::builtin::bash::BashTool::new(),
        ))
        .unwrap();
    // Only permission predicates run: the production HTTP transport never sends.
    let transport = Arc::new(HyperModelTransport::new());
    let _fetch = registry
        .register(Arc::new(WebFetchTool::new(WebFetchToolOptions {
            model: None,
            provider: None,
            model_id: None,
            max_output_tokens: None,
            fetcher: UrlFetcher::new(transport.clone()),
        })))
        .unwrap();
    let _search = registry
        .register(Arc::new(WebSearchTool::new(WebSearchOptions::new(
            transport,
        ))))
        .unwrap();
    let runtime = ToolRuntime::new(registry, Arc::new(turn.clone()), SchedulerMode::Sequential);
    let tools = runtime.list().await.unwrap();
    assert_eq!(tools.len(), 3);
    let find = |name: &str| {
        tools
            .iter()
            .find(|tool| {
                tool.fields()
                    .get("name")
                    .and_then(JsValue::as_string)
                    .is_some_and(|s| s.equals_str(name))
            })
            .unwrap()
            .clone()
    };
    for (command, expected) in [
        ("pwd", "allow"),
        ("sudo echo x", "ask"),
        ("rm -rf /", "deny"),
    ] {
        let context = turn.prepare(JsValue::Null).await.unwrap();
        let decision = turn
            .decide(
                find("bash"),
                object([("command", command.into())]),
                context,
                "call".into(),
            )
            .await
            .unwrap();
        behavior(&decision, "type", expected);
    }
    for name in ["web_fetch", "web_search"] {
        for forward in [true, false] {
            let tool = find(name);
            let tool: Arc<dyn ToolDefinition> = if forward {
                tool
            } else {
                Arc::new(MissingPermissionForwarder(tool))
            };
            let context = turn.prepare(JsValue::Null).await.unwrap();
            let decision = turn
                .decide(tool, JsValue::Null, context, "call".into())
                .await
                .unwrap();
            behavior(&decision, "type", if forward { "ask" } else { "allow" });
        }
    }
}

#[tokio::test]
async fn zero_timeout_is_an_immediate_deny_policy() {
    let resource = InteractionPermission::default();
    let (turn, _) = bind(
        &resource,
        InteractionProfileName::Interactive,
        "s",
        Arc::default(),
        Arc::new(|_| true),
    );
    let mut options = PermissionTurnOptions {
        profile: profile(InteractionProfileName::Interactive),
        context: turn.run.options.context.clone(),
        rules: RuleSetHandle::empty(),
        coordinator: Arc::default(),
        emit: Arc::new(|_| true),
        timeout: Duration::ZERO,
    };
    options.context.can_prompt = true;
    let turn = resource
        .bind_turn(
            "s".into(),
            "zero".into(),
            options,
            Arc::new(Catalog),
            &mut ModuleRouter::default(),
        )
        .unwrap();
    assert_eq!(
        prompt(&turn, Cancellation::new()).await.unwrap(),
        denied("Permission prompt timed out.")
    );
}

#[tokio::test]
async fn closing_pending_permission_queues_the_locked_discard_notice() {
    use crate::runtime::project::interaction_notice::DiscardSource;

    let coordinator = Arc::new(GatewayInteractionCoordinator::default());
    let resource = InteractionPermission::default();
    let (turn, _) = bind(
        &resource,
        InteractionProfileName::Interactive,
        "s",
        coordinator.clone(),
        Arc::new(|_| true),
    );
    let mut request = prompt(&turn, Cancellation::new());
    park(&mut request).await;
    assert_eq!(coordinator.permission_bus().pending_count(&"s".into()), 1);
    assert!(turn.take_discard_notices().is_empty());
    turn.close();
    assert!(request.await.is_err());
    let notices = turn.take_discard_notices();
    assert_eq!(notices.len(), 1);
    assert_eq!(notices[0].source, DiscardSource::Permission);
    assert_eq!(notices[0].count, 1);
    assert_eq!(notices[0].session_id, "s".into());
    assert_eq!(notices[0].run_id, "same-run".into());
    assert_eq!(
        notices[0].log_line(),
        r#"{"event":"interaction_pending_discarded","source":"permission","count":1,"sessionId":"s","runId":"same-run"}"#
    );
    assert!(turn.take_discard_notices().is_empty());
    assert_eq!(coordinator.permission_bus().pending_count(&"s".into()), 0);
    resource.dispose().await.unwrap();
}

#[tokio::test]
async fn closing_permission_without_pending_queues_no_notice() {
    let resource = InteractionPermission::default();
    let (turn, _) = bind(
        &resource,
        InteractionProfileName::Interactive,
        "s",
        Arc::default(),
        Arc::new(|_| true),
    );
    turn.close();
    assert!(turn.take_discard_notices().is_empty());
    resource.dispose().await.unwrap();
}
