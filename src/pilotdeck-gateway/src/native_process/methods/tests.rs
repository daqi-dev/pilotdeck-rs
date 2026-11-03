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
