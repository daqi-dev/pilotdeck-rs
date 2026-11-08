use super::*;
use crate::runtime::background::BackgroundTaskRuntime;

#[tokio::test]
async fn native_task_tools_use_permission_once_and_share_the_real_runtime() {
    let tasks = BackgroundTaskRuntime::default();
    let coordinator = Arc::new(GatewayInteractionCoordinator::default());
    let answerer = coordinator.clone();
    let (_scratch, tools, recorder, identity) =
        make_tools(JsObject::new(), coordinator, RuleSetHandle::empty());
    let root = tools.options.project.clone();
    std::fs::write(root.join("task.sh"), "printf TASK-OUTPUT\n").unwrap();
    let tools = tools.with_background_tasks(tasks.clone());
    let signal = Cancellation::new();
    let ports = tools.turn_with_authority(
        &identity,
        recorder,
        signal.clone(),
        Some(TurnInteractionAuthority {
            permission_mode: None,
            base_permission_mode: None,
            can_prompt: Some(true),
            policy_can_prompt: None,
            sdk_hooks: None,
            sdk_permission_mode: None,
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
    let mut router = crate::host::modules::ModuleRouter::default();
    let permission = InteractionPermission::default()
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
    let input = crate::metadata::object([("command", "bash task.sh".into())]);
    let permit = permission
        .request(
            "task_create".into(),
            "create".into(),
            input.clone(),
            vec![],
            signal.clone(),
        )
        .await;
    let context = ports
        .tool_context
        .prepare(JsValue::Undefined)
        .await
        .unwrap();
    let result = ports
        .tool_execution
        .execute_all(
            vec![ToolCall {
                id: "create".into(),
                name: "task_create".into(),
                input: input.clone(),
            }],
            context.clone(),
        )
        .await;
    let created = result.as_ref().ok().and_then(|v| v.first()).cloned();
    let task_id = created
        .as_ref()
        .and_then(|r| r.get("data"))
        .and_then(|d| d.get("taskId"))
        .cloned();
    let waited = if let Some(task_id) = task_id.clone() {
        ports
            .tool_execution
            .execute_all(
                vec![ToolCall {
                    id: "wait".into(),
                    name: "task_wait".into(),
                    input: crate::metadata::object([
                        ("taskId", task_id),
                        ("timeoutMs", JsValue::Number(3000.0)),
                    ]),
                }],
                context.clone(),
            )
            .await
    } else {
        Ok(vec![])
    };
    let replay = ports
        .tool_execution
        .execute_all(
            vec![ToolCall {
                id: "create".into(),
                name: "task_create".into(),
                input,
            }],
            context,
        )
        .await;
    // Always drain before interpreting observations: no failed assertion is
    // allowed to strand a child created by the tool under test.
    let disposed = tokio::time::timeout(Duration::from_secs(10), tasks.dispose()).await;
    permission.close();
    signal.cancel();
    assert!(disposed.unwrap().is_ok());
    assert_eq!(permit.unwrap().get("behavior"), Some(&"allow".into()));
    success(&created.expect("actual task_create result"));
    let waited = waited.unwrap();
    success(&waited[0]);
    assert!(format!("{waited:?}").contains("TASK-OUTPUT"));
    assert!(format!("{waited:?}").contains("completed"));
    assert_ne!(replay.unwrap()[0].get("type"), Some(&"success".into()));
    assert_eq!(
        tasks.list(&Default::default(), None).len(),
        1,
        "replay started a second task"
    );
}
