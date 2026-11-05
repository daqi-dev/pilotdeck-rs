use super::*;
use crate::negative_assert_eq;
use std::collections::BTreeSet;

#[test]
fn registry_matches_current_methods() {
    // Independent inventory of Requests::request at f53864ba4862. This is not
    // the advertised capability list (which also depends on runtime settings).
    let expected = [
        "snapshot_list",
        "snapshot_get",
        "snapshot_restore",
        "manager_sessions",
        "manager_browsers",
        "commands_list",
        "active_turn_snapshot",
        "run_get",
        "run_events",
        "run_reattach",
        "export_session_transcript",
        "restore_session_transcript",
        "read_session_messages",
        "fork_session",
        "replace_last_turn",
        "finalize_last_turn_replacement",
        "read_subagent_messages",
        "set_mcp_servers",
        "mcp_server_reconnect",
        "mcp_server_toggle",
        "set_mcp_permission_mode_override",
        "sop_status",
        "sop_resume",
        "steer_turn",
        "cancel_steer",
        "hook_async_result",
>>>>>>> 29c0839c7 (feat(gateway) #1307 A7-W2 S2 steer / cancel / abort + U9 接线)
        "supported_agents",
        "list_projects",
        "describe_project",
        "project_files_list",
        "project_file_read",
        "model_catalog_list",
        "session_model_get",
        "native_archive_manifest",
        "native_archive_entries",
        "native_archive_artifact",
        "usage_snapshot",
        "model_usage_snapshot",
        "mcp_server_status",
        "rewind_files",
        #[cfg(unix)]
        "background_task_stop",
        #[cfg(unix)]
        "background_tasks",
        // #1306 S1 session directory
        "list_sessions",
        "resume_session",
        "new_session",
        "close_session",
        "delete_session",
        "rename_session",
        "tag_session",
        "close_project_sessions",
        "record_agent_status_message",
    ];
    let methods: Vec<_> = registered_methods().collect();
    let actual: BTreeSet<_> = methods.iter().copied().collect();
    negative_assert_eq!(
        actual,
        expected
            .into_iter()
            .chain(
                crate::skills::SkillMethod::ALL
                    .into_iter()
                    .map(|method| method.wire())
            )
            .collect::<BTreeSet<_>>(),
        "#1305 registry contains every wired method"
    );
    negative_assert_eq!(
        methods.len(),
        actual.len(),
        "#1305 registry contains no duplicate method"
    );
}

#[tokio::test]
async fn registry_management_gate_agrees() {
    // Reuse the existing isolated fixture; keep setup failures attributable to
    // this test without accepting them as one of its mutation-kill assertions.
    let (_scratch, requests) = std::panic::catch_unwind(super::super::management_tests::setup)
        .expect("#1305 registry fixture setup");
    // Every management operation rejects this project before reading/writing
    // its resources. Some(Err) still proves the real entry gate recognized it.
    let params = crate::metadata::object([(
        "projectKey",
        JsValue::String(requests.options.home.to_string_lossy().into_owned().into()),
    )]);
    for method in
        registered_methods().chain(["unknown_method", "snapshot_unknown", "skill_unknown"])
    {
        let expected = management::METHODS.contains(&method);
        negative_assert_eq!(
            requests.management_request(method, &params).await.is_some(),
            expected,
            "#1305 management gate matches registered family"
        );
        negative_assert_eq!(
            before_admission(&requests, method, &params).await.is_some(),
            expected,
            "#1305 registry preserves management admission phase"
        );
    }
    requests
        .owner
        .shutdown()
        .await
        .expect("#1305 registry fixture owner shutdown");
    requests
        .project
        .dispose()
        .await
        .expect("#1305 registry fixture project shutdown");
}

#[tokio::test]
async fn steer_turn_rejects_attachments_explicitly() {
    // Reuse the isolated registry fixture. Without a live turn every
    // attachment-shaped input takes the ordinary no-active-turn path: TS
    // steerTurn never reads `uploadedAttachments`, and its attachment merge
    // runs after the activeTurnRunId gates (InProcessGateway.ts:1859-1870).
    // With a live turn staged, a non-empty `attachments` payload is
    // rejected explicitly instead of being silently dropped.
    use crate::runtime::turn::gateway::GatewayRequests;

    let (_scratch, requests) = std::panic::catch_unwind(super::super::management_tests::setup)
        .expect("#1307 registry fixture setup");
    let upload = || {
        JsValue::Array(vec![crate::metadata::object([
            ("type", "text".into()),
            ("text", "attachment".into()),
        ])])
    };
    let steer_params = |extra: Option<(&'static str, JsValue)>| {
        let fields: Vec<(&'static str, JsValue)> = [
            ("sessionKey", "s".into()),
            ("runId", "r".into()),
            ("itemId", "i".into()),
            ("message", "hello".into()),
        ]
        .into_iter()
        .chain(extra)
        .collect();
        crate::metadata::object(fields)
    };
    // No attachments, an explicit empty array, a non-empty
    // `uploadedAttachments` (never read by TS steerTurn), and a non-empty
    // `attachments` with no active turn: all report the ordinary
    // no-active-turn rejection in TS order (state gates before attachments).
    for params in [
        steer_params(None),
        steer_params(Some(("attachments", JsValue::Array(vec![])))),
        steer_params(Some(("uploadedAttachments", upload()))),
        steer_params(Some(("attachments", upload()))),
    ] {
        let result = requests
            .request("steer_turn".into(), params)
            .await
            .expect("#1307 state-gated path stays Ok");
        negative_assert_eq!(
            result.get("accepted"),
            Some(&JsValue::Bool(false)),
            "#1307 steer keeps no_active_turn shape"
        );
        negative_assert_eq!(
            result.get("reason").and_then(JsValue::as_string),
            Some(&pilotdeck_protocol::JsString::from("no_active_turn")),
            "#1307 attachments are handled after the state gates"
        );
    }
    // Stage the state both gates require: an admission slot plus a live steer
    // run with a matching run ID.
    let _reservation = requests
        .owner
        .begin_turn(&"s".into(), &"r".into())
        .expect("#1307 admission slot");
    let steer = {
        let lease = requests.project.acquire(None).expect("#1307 project lease");
        lease
            .runtime()
            .resources()
            .get::<crate::runtime::project::ProjectResources>()
            .expect("#1307 project resources")
            .context()
            .resource(crate::runtime::project::Resource::InteractionSteer)
            .expect("#1307 interaction steer resource")
            .as_any()
            .downcast_ref::<crate::runtime::project::interaction_steer::InteractionSteer>()
            .expect("#1307 steer resource type")
            .clone()
    };
    steer
        .bind_session("s".into(), None)
        .expect("#1307 steer session binding")
        .start_turn(
            "t".into(),
            "r".into(),
            vec![],
            crate::runtime::hooks::Cancellation::new(),
            &mut crate::host::modules::ModuleRouter::default(),
        )
        .expect("#1307 steer turn start");
    // With a live turn, a non-empty `attachments` payload is rejected with the
    // domain error rather than a fake accepted.
    let error = requests
        .request(
            "steer_turn".into(),
            steer_params(Some(("attachments", upload()))),
        )
        .await
        .expect_err("#1307 attachments must be rejected with a live turn");
    negative_assert_eq!(
        error.code,
        "STEER_ATTACHMENTS_UNSUPPORTED",
        "#1307 attachments rejection uses the domain code"
    );
    // A non-empty `uploadedAttachments` stays accepted-and-ignored with a live
    // turn: TS steerTurn never reads the field, so the steer itself proceeds.
    let result = requests
        .request(
            "steer_turn".into(),
            steer_params(Some(("uploadedAttachments", upload()))),
        )
        .await
        .expect("#1307 uploadedAttachments stays Ok with a live turn");
    negative_assert_eq!(
        result.get("accepted"),
        Some(&JsValue::Bool(true)),
        "#1307 uploadedAttachments does not block the steer"
    );
    requests
        .owner
        .shutdown()
        .await
        .expect("#1307 registry fixture owner shutdown");
    requests
        .project
        .dispose()
        .await
        .expect("#1307 registry fixture project shutdown");
}

#[tokio::test]
async fn hook_async_result_validates_ids_before_registry_lookup() {
    // The fixture stages no async-hook registry; TS submitAsyncHookResult
    // validates blank ids unconditionally before any lookup
    // (InProcessGateway.ts:1896-1899), so blank ids must be rejected on the
    // registry-less path too instead of reporting `unknown`.
    use crate::runtime::turn::gateway::GatewayRequests;

    let (_scratch, requests) = std::panic::catch_unwind(super::super::management_tests::setup)
        .expect("#1307 registry fixture setup");
    let hook_params = |session_key: &str, invocation_id: &str| {
        crate::metadata::object([
            ("sessionKey", session_key.into()),
            ("invocationId", invocation_id.into()),
        ])
    };
    for params in [
        hook_params("", "i"),
        hook_params("  ", "i"),
        hook_params("s", ""),
        hook_params("s", "\t"),
    ] {
        let error = requests
            .request("hook_async_result".into(), params)
            .await
            .expect_err("#1307 blank ids must be rejected without a registry");
        negative_assert_eq!(
            error.code,
            "INVALID_ASYNC_HOOK_RESULT",
            "#1307 blank ids use the TS domain code"
        );
    }
    // Non-blank ids without a registry keep the unknown shape, not an error.
    let result = requests
        .request("hook_async_result".into(), hook_params("s", "i"))
        .await
        .expect("#1307 registry-less lookup stays Ok");
    negative_assert_eq!(
        result.get("status").and_then(JsValue::as_string),
        Some(&pilotdeck_protocol::JsString::from("unknown")),
        "#1307 non-blank ids without a registry report unknown"
    );
    negative_assert_eq!(
        result.get("invocationId").and_then(JsValue::as_string),
        Some(&pilotdeck_protocol::JsString::from("i")),
        "#1307 unknown result echoes the invocationId"
    );
    requests
        .owner
        .shutdown()
        .await
        .expect("#1307 registry fixture owner shutdown");
    requests
        .project
        .dispose()
        .await
        .expect("#1307 registry fixture project shutdown");
}
