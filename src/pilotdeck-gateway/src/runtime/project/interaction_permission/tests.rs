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
    Arc::new(PermissionDefinition(Arc::new(
        crate::runtime::tools::builtin::bash::BashTool::new(),
    )))
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
            let golden = pilotdeck_protocol::parse_json(include_str!(
                "../../../../tests/fixtures/interaction-permission/oracle.json"
            ))
            .unwrap();
            let JsValue::Array(ref golden) = golden else {
                panic!("array oracle")
            };
            assert_eq!(
                &result,
                golden[0]
                    .get("result")
                    .unwrap()
                    .get("specific")
                    .unwrap()
                    .get("decision")
                    .unwrap()
            );
        }
        assert_eq!(
            turn.run.options.rules.snapshot().allow.len(),
            usize::from(name == InteractionProfileName::Interactive)
        );
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
