use super::*;

fn read_tool() -> Arc<dyn ToolDefinition> {
    crate::runtime::tools::builtin::read_only_tools()
        .into_iter()
        .find(|t| t.fields().get("name") == Some(&"read_file".into()))
        .unwrap()
}
fn input(file: &str) -> JsValue {
    object([("file_path", file.into())])
}

#[tokio::test]
async fn native_once_authorization_matches_call_input_and_rechecks_deny() {
    let coordinator = Arc::new(GatewayInteractionCoordinator::default());
    let answerer = coordinator.clone();
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
                    decision: "allow".into(),
                    remember: None,
                    reason: None,
                },
                None,
            )
        }),
    );
    turn.enable_native_execution().unwrap();
    turn.run.options.rules.update_member("ask", |r| {
        r.push(PermissionRule::new("project", "ask", "read_file").into())
    });
    let answer = turn
        .request(
            "read_file".into(),
            "call".into(),
            input("a"),
            vec![],
            Cancellation::new(),
        )
        .await
        .unwrap();
    behavior(&answer, "behavior", "allow");
    behavior(
        &turn
            .decide_for_execution(read_tool(), input("b"), "call".into())
            .await
            .unwrap(),
        "type",
        "ask",
    );
    behavior(
        &turn
            .decide_for_execution(read_tool(), input("a"), "other-id".into())
            .await
            .unwrap(),
        "type",
        "ask",
    );
    behavior(
        &turn
            .decide_for_execution(read_tool(), input("a"), "call".into())
            .await
            .unwrap(),
        "type",
        "allow",
    );
    behavior(
        &turn
            .decide_for_execution(read_tool(), input("a"), "call".into())
            .await
            .unwrap(),
        "type",
        "ask",
    );
    turn.request(
        "read_file".into(),
        "second".into(),
        input("a"),
        vec![],
        Cancellation::new(),
    )
    .await
    .unwrap();
    turn.run.options.rules.update_member("deny", |r| {
        r.push(PermissionRule::new("project", "deny", "read_file").into())
    });
    behavior(
        &turn
            .decide_for_execution(read_tool(), input("a"), "second".into())
            .await
            .unwrap(),
        "type",
        "deny",
    );
    turn.close();
    assert!(lock(&turn.run.native_grants).is_empty());
}

#[tokio::test]
async fn deny_cancel_and_replacement_cannot_leave_an_execution_grant() {
    let coordinator = Arc::new(GatewayInteractionCoordinator::default());
    let answerer = coordinator.clone();
    let resource = InteractionPermission::default();
    let (denied_turn, _) = bind(
        &resource,
        InteractionProfileName::Interactive,
        "denied",
        coordinator.clone(),
        Arc::new(move |event| {
            answerer.decide_permission(
                &"denied".into(),
                crate::runtime::interaction::PermissionDecision {
                    request_id: event.get("requestId").unwrap().as_string().unwrap().clone(),
                    decision: "deny".into(),
                    remember: None,
                    reason: None,
                },
                None,
            )
        }),
    );
    denied_turn.enable_native_execution().unwrap();
    behavior(
        &denied_turn
            .request(
                "read_file".into(),
                "call".into(),
                input("a"),
                vec![],
                Cancellation::new(),
            )
            .await
            .unwrap(),
        "behavior",
        "deny",
    );
    assert!(lock(&denied_turn.run.native_grants).is_empty());
    let (old, _) = bind(
        &resource,
        InteractionProfileName::Interactive,
        "s",
        coordinator.clone(),
        Arc::new(|_| true),
    );
    old.enable_native_execution().unwrap();
    let cancel = Cancellation::new();
    let mut pending = old.request(
        "read_file".into(),
        "call".into(),
        input("a"),
        vec![],
        cancel.clone(),
    );
    park(&mut pending).await;
    assert_eq!(coordinator.permission_bus().pending_count(&"s".into()), 1);
    cancel.cancel();
    behavior(&pending.await.unwrap(), "behavior", "deny");
    assert_eq!(coordinator.permission_bus().pending_count(&"s".into()), 0);
    let (new, _) = bind(
        &resource,
        InteractionProfileName::Interactive,
        "s",
        coordinator,
        Arc::new(|_| true),
    );
    new.enable_native_execution().unwrap();
    assert!(
        old.decide_for_execution(read_tool(), input("a"), "call".into())
            .await
            .is_err()
    );
    assert!(lock(&new.run.native_grants).is_empty());
    resource.close();
}

#[tokio::test]
async fn session_grant_updates_the_same_registry_handle_used_by_execution() {
    struct Fixture(std::path::PathBuf);
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    let fixture =
        Fixture(std::env::temp_dir().join(format!("pdrs-session-grant-{}", uuid::Uuid::new_v4())));
    std::fs::create_dir(&fixture.0).unwrap();
    std::fs::write(fixture.0.join("a"), b"session grant fixture\n").unwrap();
    let registry =
        Arc::new(crate::config::permission::GatewaySessionPermissionRuleSetRegistry::new());
    let lease = registry.acquire(&"s".into(), None);
    let rules = lease.rules();
    rules.update_member("deny", |r| {
        r.push(PermissionRule::new("user", "deny", "read_file").into())
    });
    let coordinator = Arc::new(GatewayInteractionCoordinator::new(
        crate::runtime::interaction::InteractionOptions {
            permission_grants: Some(registry),
            ..Default::default()
        },
    ));
    let resource = InteractionPermission::default();
    let mut router = ModuleRouter::default();
    let turn = resource
        .bind_turn(
            "s".into(),
            "run".into(),
            PermissionTurnOptions {
                profile: profile(InteractionProfileName::Interactive),
                context: permission::create_default_permission_context(
                    permission::DefaultPermissionContextOptions {
                        cwd: fixture.0.to_string_lossy().into_owned().into(),
                        can_prompt: Some(true),
                        ..Default::default()
                    },
                ),
                rules,
                coordinator: coordinator.clone(),
                emit: Arc::new(|_| true),
                timeout: Duration::from_secs(1),
            },
            Arc::new(Catalog),
            &mut router,
        )
        .unwrap();
    turn.enable_native_execution().unwrap();
    behavior(
        &turn
            .decide_for_execution(read_tool(), input("a"), "before".into())
            .await
            .unwrap(),
        "type",
        "deny",
    );
    coordinator.grant_session_permission(Some(&"s".into()), &"read_file".into());
    behavior(
        &turn
            .decide_for_execution(read_tool(), input("a"), "after".into())
            .await
            .unwrap(),
        "type",
        "allow",
    );
    turn.close();
    lease.release();
}
