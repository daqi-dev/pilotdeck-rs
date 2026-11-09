use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};
#[tokio::test]
async fn native_skill_ask_grant_is_single_use_and_deny_cannot_read_the_catalog() {
    for choice in ["allow", "deny"] {
        let coordinator = Arc::new(GatewayInteractionCoordinator::default());
        let answerer = coordinator.clone();
        let prompts = Arc::new(AtomicUsize::new(0));
        let emitted = prompts.clone();
        let rules = RuleSetHandle::empty();
        rules.update_member("ask", |rules| {
            rules.push(PermissionRule::new("project", "ask", "read_skill").into())
        });
        let (_scratch, mut tools, recorder, identity) =
            make_tools(JsObject::new(), coordinator.clone(), rules);
        tools.skills = crate::native_process::skills::NativeSkills::from_contributions(&[
            crate::runtime::plugins::PluginSkillContribution {
                name: "demo".into(),
                description: None,
                path: "/fixture/demo/SKILL.md".into(),
                content: "SKILL-PERMISSION-BODY".into(),
                namespace: "demo".into(),
            },
        ]);
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
                    emitted.fetch_add(1, Ordering::SeqCst);
                    answerer.decide_permission(
                        &"session".into(),
                        crate::runtime::interaction::PermissionDecision {
                            request_id: event
                                .get("requestId")
                                .unwrap()
                                .as_string()
                                .unwrap()
                                .clone(),
                            decision: choice.into(),
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
        let input = crate::metadata::object([("skillName", "demo".into())]);
        let answer = permission
            .request(
                "read_skill".into(),
                "skill-1".into(),
                input.clone(),
                vec![],
                Cancellation::new(),
            )
            .await
            .unwrap();
        assert_eq!(answer.get("behavior"), Some(&JsValue::from(choice)));
        let invoke = ports
            .tool_context
            .prepare(JsValue::Undefined)
            .await
            .unwrap();
        let result = ports
            .tool_execution
            .execute_all(
                vec![ToolCall {
                    id: "skill-1".into(),
                    name: "read_skill".into(),
                    input: input.clone(),
                }],
                invoke,
            )
            .await
            .unwrap();
        assert_eq!(
            result[0].get("type") == Some(&"success".into()),
            choice == "allow"
        );
        let invoke = ports
            .tool_context
            .prepare(JsValue::Undefined)
            .await
            .unwrap();
        let replay = ports
            .tool_execution
            .execute_all(
                vec![ToolCall {
                    id: "skill-1".into(),
                    name: "read_skill".into(),
                    input,
                }],
                invoke,
            )
            .await
            .unwrap();
        assert_ne!(replay[0].get("type"), Some(&"success".into()));
        assert_eq!(prompts.load(Ordering::SeqCst), 1);
        assert_eq!(
            coordinator
                .permission_bus()
                .pending_count(&"session".into()),
            0
        );
        permission.close();
    }
}
