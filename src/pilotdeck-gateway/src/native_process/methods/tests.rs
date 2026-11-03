use super::*;
use crate::{negative_assert, negative_assert_eq};
use std::collections::BTreeSet;

/// The project's staged `InteractionSteer` resource for the registry fixture.
fn steer_resource(
    requests: &super::super::requests::Requests,
) -> crate::runtime::project::interaction_steer::InteractionSteer {
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
}

/// Bind `session` and start a live steer turn (`turn` / `run`) on it.
fn start_steer_run(
    steer: &crate::runtime::project::interaction_steer::InteractionSteer,
    session: &str,
    turn: &str,
    run: &str,
) -> crate::runtime::project::interaction_steer::TurnSteer {
    steer
        .bind_session(session.into(), None)
        .expect("#1307 steer session binding")
        .start_turn(
            turn.into(),
            run.into(),
            vec![],
            crate::runtime::hooks::Cancellation::new(),
            &mut crate::host::modules::ModuleRouter::default(),
        )
        .expect("#1307 steer turn start")
}

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
        "memory_list",
        "memory_wipe",
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
        "permission_list",
        "grant_session_permission",
        "user_dialog_list",
        "user_dialog_claim",
        "user_dialog_release",
        "user_dialog_respond",
        // #1310 A7-W5 S5 session configuration methods
        "session_model_set",
        "session_model_clear",
        "set_permission_mode",
        "set_session_thinking",
        "output_styles_list",
        "set_output_style",
        "seed_read_state",
        "upload_create",
        "upload_get",
        "upload_part",
        "upload_complete",
        "upload_cancel",
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
    let steer = steer_resource(&requests);
    let _turn = start_steer_run(&steer, "s", "t", "r");
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

#[tokio::test]
async fn steer_turn_attachments_sit_between_admission_and_resource_gates() {
    // TS steerTurn order (re-read this round, InProcessGateway.ts:1859-1883):
    // the two activeTurnRunId branches (:1860-1862) come first, attachments
    // are prepared immediately after (:1864-1870), and the session/run
    // resource gates only run inside router.steer (:1878-1883 →
    // SessionRouter.ts:307-314, AgentSession.ts:290-297). The slice's
    // explicit attachments rejection must therefore fire once the admission
    // branches pass — even when the steer-side resource gate would still
    // have reported no_active_turn — while turn_mismatch still wins outright.
    use crate::runtime::turn::gateway::GatewayRequests;

    let (_scratch, requests) = std::panic::catch_unwind(super::super::management_tests::setup)
        .expect("#1307 registry fixture setup");
    let upload = || {
        JsValue::Array(vec![crate::metadata::object([
            ("type", "text".into()),
            ("text", "attachment".into()),
        ])])
    };
    let steer_params = |run_id: &str, attachments: bool| {
        let fields: Vec<(&'static str, JsValue)> = [
            ("sessionKey", "s".into()),
            ("runId", run_id.into()),
            ("itemId", "i".into()),
            ("message", "hello".into()),
        ]
        .into_iter()
        .chain(attachments.then(|| ("attachments", upload())))
        .collect();
        crate::metadata::object(fields)
    };
    // turn_mismatch precedes the attachments rejection: the admission branch
    // fires first (InProcessGateway.ts:1862).
    let _reservation = requests
        .owner
        .begin_turn(&"s".into(), &"r".into())
        .expect("#1307 admission slot");
    let result = requests
        .request("steer_turn".into(), steer_params("x", true))
        .await
        .expect("#1307 mismatch branch stays Ok");
    negative_assert_eq!(
        result.get("accepted"),
        Some(&JsValue::Bool(false)),
        "#1307 turn_mismatch branch precedes attachments"
    );
    negative_assert_eq!(
        result.get("reason").and_then(JsValue::as_string),
        Some(&pilotdeck_protocol::JsString::from("turn_mismatch")),
        "#1307 mismatched run reports turn_mismatch, not the attachments error"
    );
    // Admission matched but no steer run staged: TS would still have prepared
    // the attachments before router.steer reports no_active_turn, so the
    // slice's explicit rejection fires here.
    let error = requests
        .request("steer_turn".into(), steer_params("r", true))
        .await
        .expect_err("#1307 attachments rejected before the resource gate");
    negative_assert_eq!(
        error.code,
        "STEER_ATTACHMENTS_UNSUPPORTED",
        "#1307 attachments rejection precedes the resource gate"
    );
    // Same when a steer run exists but its run ID disagrees with the
    // admission slot: that resource gate only runs after attachment prep.
    let steer = steer_resource(&requests);
    let _turn = start_steer_run(&steer, "s", "t", "other");
    let error = requests
        .request("steer_turn".into(), steer_params("r", true))
        .await
        .expect_err("#1307 attachments rejected before the slot-mismatch gate");
    negative_assert_eq!(
        error.code,
        "STEER_ATTACHMENTS_UNSUPPORTED",
        "#1307 attachments rejection precedes the slot-mismatch gate"
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
async fn steer_turn_uploads_never_reach_the_canonical_message() {
    // TS steerTurn never reads `uploadedAttachments` (only submitTurn
    // consumes uploads; InProcessGateway.ts:1864-1870). Drain the mailbox
    // after a steer carrying uploads and pin the delivered message: the
    // canonical message, its content, and allowedReadFiles must be exactly
    // the no-upload shape.
    use crate::runtime::turn::gateway::GatewayRequests;

    let (_scratch, requests) = std::panic::catch_unwind(super::super::management_tests::setup)
        .expect("#1307 registry fixture setup");
    let _reservation = requests
        .owner
        .begin_turn(&"s".into(), &"r".into())
        .expect("#1307 admission slot");
    let steer = steer_resource(&requests);
    let _turn = start_steer_run(&steer, "s", "t", "r");
    let upload = || {
        JsValue::Array(vec![crate::metadata::object([
            ("type", "text".into()),
            ("text", "upload body".into()),
        ])])
    };
    let result = requests
        .request(
            "steer_turn".into(),
            crate::metadata::object([
                ("sessionKey", "s".into()),
                ("runId", "r".into()),
                ("itemId", "i".into()),
                ("message", "hello".into()),
                ("uploadedAttachments", upload()),
            ]),
        )
        .await
        .expect("#1307 uploadedAttachments stays Ok with a live turn");
    negative_assert_eq!(
        result.get("accepted"),
        Some(&JsValue::Bool(true)),
        "#1307 uploads do not block the steer"
    );
    let drained = steer
        .mailbox_for_test(&"s".into())
        .expect("#1307 session mailbox")
        .drain(&"t".into());
    negative_assert_eq!(
        drained.len(),
        1,
        "#1307 exactly the steer message is queued"
    );
    let delivered = &drained[0];
    negative_assert_eq!(
        delivered.item_id,
        pilotdeck_protocol::JsString::from("i"),
        "#1307 delivered item keeps its id"
    );
    negative_assert_eq!(
        delivered.message.get("role"),
        Some(&JsValue::String("user".into())),
        "#1307 canonical role is user"
    );
    negative_assert_eq!(
        delivered.message.get("content"),
        Some(&JsValue::Array(vec![crate::metadata::object([
            ("type", "text".into()),
            ("text", "hello".into()),
        ])])),
        "#1307 content is the plain text message, uploads are not merged"
    );
    let metadata = delivered
        .message
        .get("metadata")
        .expect("#1307 canonical metadata");
    negative_assert_eq!(
        metadata.get("purpose"),
        Some(&JsValue::String("mid_turn_steer".into())),
        "#1307 steer purpose is preserved"
    );
    negative_assert_eq!(
        metadata.get("queueItemId"),
        Some(&JsValue::String("i".into())),
        "#1307 queue item id is preserved"
    );
    negative_assert_eq!(
        delivered.allowed_read_files,
        Some(Vec::new()),
        "#1307 allowedReadFiles stay empty without attachments"
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
async fn steer_turn_cannot_cross_runs_after_a_same_text_turn_restart() {
    // Gate-r4 concurrency race, staged with the handler's test interleave:
    // (1) steer_turn passes the admission gate and captures the steer info
    // (run r1, turn "t"); (2) r1 ends and the mailbox turn is closed;
    // (3) a retry restarts a new turn r2 under the same textual turn ID
    // (mailbox.start resets open=true); (4) the handler resumes and
    // enqueues. The enqueue must be rejected by run identity under the
    // admission lock — never by textual turn-ID comparison — or the message
    // lands in the new run with accepted:true (cross-run delivery).
    use crate::runtime::turn::gateway::GatewayRequests;

    let (_scratch, requests) = std::panic::catch_unwind(super::super::management_tests::setup)
        .expect("#1307 registry fixture setup");
    let session = pilotdeck_protocol::JsString::from("race");
    let reservation = requests
        .owner
        .begin_turn(&session, &"r1".into())
        .expect("#1307 admission slot");
    let steer = steer_resource(&requests);
    let binding = steer
        .bind_session(session.clone(), None)
        .expect("#1307 steer session binding");
    let first = binding
        .start_turn(
            "t".into(),
            "r1".into(),
            vec![],
            crate::runtime::hooks::Cancellation::new(),
            &mut crate::host::modules::ModuleRouter::default(),
        )
        .expect("#1307 first steer turn start");
    // Suspend the request between its gates and the enqueue.
    let (reached, resume) = super::steer::race_hook::arm(session.clone());
    let requester = requests.clone();
    let request_session = session.clone();
    let request = tokio::spawn(async move {
        requester
            .request(
                "steer_turn".into(),
                crate::metadata::object([
                    ("sessionKey", JsValue::String(request_session)),
                    ("runId", "r1".into()),
                    ("itemId", "i".into()),
                    ("message", "hello".into()),
                ]),
            )
            .await
    });
    reached.await.expect("#1307 handler reached the interleave");
    // (2) r1 ends; (3) the retry restarts the turn under the same turn ID.
    first
        .close()
        .await
        .expect("#1307 first turn closed and drained");
    requests
        .owner
        .end_turn(&session, Some(&"r1".into()), Some(&reservation));
    let _restart = requests
        .owner
        .begin_turn(&session, &"r2".into())
        .expect("#1307 restarted admission slot");
    let _second = binding
        .start_turn(
            "t".into(),
            "r2".into(),
            vec![],
            crate::runtime::hooks::Cancellation::new(),
            &mut crate::host::modules::ModuleRouter::default(),
        )
        .expect("#1307 restarted turn start");
    resume.send(true).expect("#1307 resume the paused handler");
    // (4) the resumed steer must be rejected, not delivered to the new run.
    let result = request
        .await
        .expect("#1307 request task joined")
        .expect("#1307 stale steer stays Ok");
    negative_assert_eq!(
        result.get("accepted"),
        Some(&JsValue::Bool(false)),
        "#1307 no cross-run delivery after a same-text turn restart"
    );
    negative_assert_eq!(
        result.get("reason").and_then(JsValue::as_string),
        Some(&pilotdeck_protocol::JsString::from("turn_closing")),
        "#1307 stale steer reports turn_closing"
    );
    let drained = steer
        .mailbox_for_test(&session)
        .expect("#1307 session mailbox")
        .drain(&"t".into());
    negative_assert_eq!(
        drained.len(),
        0,
        "#1307 the restarted turn received nothing"
    );
    // Positive control: the restarted turn still accepts a fresh steer.
    let fresh = requests
        .request(
            "steer_turn".into(),
            crate::metadata::object([
                ("sessionKey", JsValue::String(session.clone())),
                ("runId", "r2".into()),
                ("itemId", "i2".into()),
                ("message", "fresh".into()),
            ]),
        )
        .await
        .expect("#1307 restarted turn stays steerable");
    negative_assert_eq!(
        fresh.get("accepted"),
        Some(&JsValue::Bool(true)),
        "#1307 the restarted turn still accepts a fresh steer"
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
async fn hook_async_result_blank_id_error_shape_is_pinned_across_registry_states() {
    // TS submitAsyncHookResult throws DialogGatewayError(
    // "INVALID_ASYNC_HOOK_RESULT", "sessionKey and invocationId are
    // required.") with no details and no validation payload
    // (InProcessGateway.ts:1896-1899, re-read this round). Pin the complete
    // error shape in both registry states: the blank-id validation runs
    // before any registry lookup, so neither state may reshape it.
    use crate::runtime::async_hooks::{AsyncHookRegistry, AsyncHookTurn, TokioHookClock};
    use crate::runtime::steer::SteerMailbox;
    use crate::runtime::turn::gateway::GatewayRequests;

    let (_scratch, requests) = std::panic::catch_unwind(super::super::management_tests::setup)
        .expect("#1307 registry fixture setup");
    let mut with_registry = requests.clone();
    let mailbox = SteerMailbox::new(None);
    mailbox.start("t".into());
    with_registry.async_hook_registry = Some(std::sync::Arc::new(AsyncHookRegistry::new(
        std::sync::Arc::new(TokioHookClock),
        move |_: &pilotdeck_protocol::JsString| {
            Some(AsyncHookTurn {
                run_id: "t".into(),
                mailbox: mailbox.clone(),
            })
        },
        |_, _| unreachable!("blank ids must never reach the registry sink"),
    )));
    let hook_params = |session_key: &str, invocation_id: &str| {
        crate::metadata::object([
            ("sessionKey", session_key.into()),
            ("invocationId", invocation_id.into()),
        ])
    };
    for (state, registry_requests) in [
        ("registry-less", &requests),
        ("registry staged", &with_registry),
    ] {
        for (session_key, invocation_id) in [("", "i"), ("  ", "i"), ("s", ""), ("s", "\t")] {
            let error = registry_requests
                .request(
                    "hook_async_result".into(),
                    hook_params(session_key, invocation_id),
                )
                .await
                .expect_err("#1307 blank ids rejected in both registry states");
            negative_assert_eq!(
                error.code,
                "INVALID_ASYNC_HOOK_RESULT",
                "#1307 blank ids use the TS domain code ({state})"
            );
            negative_assert_eq!(
                error.message,
                pilotdeck_protocol::JsString::from("sessionKey and invocationId are required."),
                "#1307 blank ids echo the TS message ({state})"
            );
            negative_assert_eq!(
                error.details,
                None,
                "#1307 blank-id error carries no details ({state})"
            );
            negative_assert_eq!(
                error.validation,
                None,
                "#1307 blank-id error carries no validation payload ({state})"
            );
        }
    }
    // The staged-registry state is genuinely wired: non-blank ids reach the
    // registry and report unknown, not an error.
    let result = with_registry
        .request("hook_async_result".into(), hook_params("s", "i"))
        .await
        .expect("#1307 staged-registry lookup stays Ok");
    negative_assert_eq!(
        result.get("status").and_then(JsValue::as_string),
        Some(&pilotdeck_protocol::JsString::from("unknown")),
        "#1307 staged registry reports unknown for unregistered ids"
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

// #1309 fix-r4 (gate P2a): permission family domain error codes and the
// user_dialog ttlMs null → default-lease TS semantics (InProcessGateway.ts:2741).
fn message(error: &crate::runtime::connection::RequestError) -> String {
    error.message.to_utf8().expect("#1309 error message UTF-8")
}

#[tokio::test]
async fn permission_errors_carry_domain_codes() {
    let (_scratch, requests) = std::panic::catch_unwind(super::super::management_tests::setup)
        .expect("#1309 permission fixture setup");
    // permission_list — TS: DialogGatewayError("INVALID_PERMISSION_LIST",
    // "sessionKey is required.") (InProcessGateway.ts:2782); the code must
    // travel in RequestError::code, not be embedded in the message.
    let error = requests
        .permission_request("permission_list".into(), crate::metadata::object([]))
        .await
        .expect_err("#1309 permission_list rejects missing sessionKey");
    negative_assert_eq!(
        error.code,
        "INVALID_PERMISSION_LIST",
        "#1309 permission_list error code is the domain code"
    );
    negative_assert_eq!(
        message(&error),
        "sessionKey is required.",
        "#1309 permission_list message has no code prefix"
    );

    // grant_session_permission — schema-required sessionKey/entry; TS has no
    // input validation (GatewayInteractionCoordinator.ts:128), so these are the
    // Rust-side contract rejects and keep their own domain code.
    let error = requests
        .permission_request(
            "grant_session_permission".into(),
            crate::metadata::object([("entry", "Bash".into())]),
        )
        .await
        .expect_err("#1309 grant rejects missing sessionKey");
    negative_assert_eq!(
        error.code,
        "INVALID_PERMISSION_GRANT",
        "#1309 grant error code is the domain code"
    );
    negative_assert_eq!(
        message(&error),
        "sessionKey is required.",
        "#1309 grant sessionKey message has no code prefix"
    );

    let error = requests
        .permission_request(
            "grant_session_permission".into(),
            crate::metadata::object([
                ("sessionKey", "p2a-session".into()),
                ("entry", JsValue::Number(1.0)),
            ]),
        )
        .await
        .expect_err("#1309 grant rejects non-string entry");
    negative_assert_eq!(
        error.code,
        "INVALID_PERMISSION_GRANT",
        "#1309 grant non-string entry code is the domain code"
    );
    negative_assert_eq!(
        message(&error),
        "entry must be a string.",
        "#1309 grant entry message has no code prefix"
    );
    requests
        .owner
        .shutdown()
        .await
        .expect("#1309 permission fixture owner shutdown");
    requests
        .project
        .dispose()
        .await
        .expect("#1309 permission fixture project shutdown");
}

#[tokio::test]
async fn user_dialog_capability_gate_precedes_parameter_validation() {
    // The setup bus is unwired (no on_change observer), so the capability
    // error must win over the missing-sessionKey domain error — the shared
    // require_user_dialog_bus gate runs before any parameter validation.
    let (_scratch, requests) = std::panic::catch_unwind(super::super::management_tests::setup)
        .expect("#1309 gate-order fixture setup");
    for method in [
        "user_dialog_list",
        "user_dialog_claim",
        "user_dialog_release",
    ] {
        let error = requests
            .user_dialog_request(method.into(), crate::metadata::object([]))
            .await
            .expect_err("#1309 unwired bus is a capability error");
        negative_assert_eq!(
            error.code,
            "CAPABILITY_UNAVAILABLE",
            "#1309 {method} capability gate precedes parameter validation"
        );
    }
    requests
        .owner
        .shutdown()
        .await
        .expect("#1309 gate-order fixture owner shutdown");
    requests
        .project
        .dispose()
        .await
        .expect("#1309 gate-order fixture project shutdown");
}

#[tokio::test]
async fn user_dialog_claim_null_ttlms_claims_default_lease() {
    let (_scratch, mut requests) = std::panic::catch_unwind(super::super::management_tests::setup)
        .expect("#1309 ttlMs fixture setup");
    let session: JsString = "p2a-session".into();
    let request: JsString = "p2a-dialog".into();
    // Wire a connected bus (on_change supplied → is_connected) and register a
    // pending dialog so the claim can succeed.
    requests.user_dialog_bus = std::sync::Arc::new(std::sync::Mutex::new(
        crate::user_dialog::GatewayUserDialogBus::new(
            crate::user_dialog::DialogRuntime::default(),
            Some(Box::new(|_event| Ok(()))),
        ),
    ));
    requests
        .user_dialog_bus
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .register(
            &session,
            crate::user_dialog::PendingDialog {
                request_id: request.clone(),
                dialog_kind: "question".into(),
                tool_call_id: "call-1".into(),
                tool_name: "ask".into(),
                event: crate::metadata::object([("requestId", JsValue::String(request.clone()))]),
                accepts: Box::new(|_value| true),
                resolve: Box::new(|_answer| {}),
                reject: Box::new(|_error| {}),
                on_settled: None,
            },
        );
    // TS `input.ttlMs ?? DEFAULT_USER_DIALOG_LEASE_MS`: an explicit null
    // claims with the default 30s lease instead of failing validation.
    let result = requests
        .user_dialog_request(
            "user_dialog_claim".into(),
            crate::metadata::object([
                ("sessionKey", JsValue::String(session.clone())),
                ("requestId", JsValue::String(request.clone())),
                ("ttlMs", JsValue::Null),
            ]),
        )
        .await
        .expect("#1309 ttlMs null claims with the default lease");
    negative_assert_eq!(
        result.get("claimed"),
        Some(&JsValue::Bool(true)),
        "#1309 ttlMs null claim succeeds"
    );
    negative_assert!(
        result
            .get("leaseId")
            .and_then(JsValue::as_string)
            .is_some_and(|lease| !lease.to_utf8().unwrap_or_default().is_empty()),
        "#1309 ttlMs null claim returns a lease"
    );
    // A non-number ttlMs is still a domain validation error; only null and
    // missing fall back to the default lease.
    let error = requests
        .user_dialog_request(
            "user_dialog_claim".into(),
            crate::metadata::object([
                ("sessionKey", JsValue::String(session.clone())),
                ("requestId", JsValue::String(request)),
                ("ttlMs", "30000".into()),
            ]),
        )
        .await
        .expect_err("#1309 non-number ttlMs is rejected");
    negative_assert_eq!(
        error.code,
        "INVALID_USER_DIALOG_CLAIM",
        "#1309 string ttlMs keeps the domain rejection"
    );
    requests
        .owner
        .shutdown()
        .await
        .expect("#1309 ttlMs fixture owner shutdown");
    requests
        .project
        .dispose()
        .await
        .expect("#1309 ttlMs fixture project shutdown");
}

// #1309 fix-r5 (gate P2a x2): explicit-null tri-state for the user_dialog
// optional fields, pinned to the TS guards in InProcessGateway.ts (claim
// leaseId :2738, respond leaseId :2655, cancelled reason :2652, answered
// value :2649 judged by accepts :2674). Raw TS probe output:
// main-repo _tmp/20261105-issue-1309-fix-r5/ts-probe-output.txt.
fn register_null_tri_state_dialog(
    requests: &super::super::requests::Requests,
    session: &JsString,
    request_id: &str,
    accepts_null: bool,
    resolved: &std::sync::Arc<std::sync::Mutex<Vec<crate::user_dialog::DialogAnswer>>>,
) {
    let sink = resolved.clone();
    requests
        .user_dialog_bus
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .register(
            session,
            crate::user_dialog::PendingDialog {
                request_id: request_id.into(),
                dialog_kind: "question".into(),
                tool_call_id: "call-1".into(),
                tool_name: "ask".into(),
                event: crate::metadata::object([("requestId", JsValue::String(request_id.into()))]),
                accepts: Box::new(move |value| std::matches!(value, JsValue::Null) == accepts_null),
                resolve: Box::new(move |answer| sink.lock().unwrap().push(answer)),
                reject: Box::new(|_error| {}),
                on_settled: None,
            },
        );
}

#[tokio::test]
async fn user_dialog_optional_nulls_match_typescript_tri_state() {
    let (_scratch, mut requests) = std::panic::catch_unwind(super::super::management_tests::setup)
        .expect("#1309 fix-r5 fixture setup");
    let session: JsString = "p2a-r5-session".into();
    let resolved: std::sync::Arc<std::sync::Mutex<Vec<crate::user_dialog::DialogAnswer>>> =
        std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    requests.user_dialog_bus = std::sync::Arc::new(std::sync::Mutex::new(
        crate::user_dialog::GatewayUserDialogBus::new(
            crate::user_dialog::DialogRuntime::default(),
            Some(Box::new(|_event| Ok(()))),
        ),
    ));

    // claim leaseId: null -> rejected, same as TS
    // `input.leaseId !== undefined && (typeof input.leaseId !== "string" || ...)`
    // (InProcessGateway.ts:2738). Unlike ttlMs (`??`, :2741), null does NOT
    // fold into the missing-key default.
    register_null_tri_state_dialog(&requests, &session, "r5-claim-null-lease", true, &resolved);
    let error = requests
        .user_dialog_request(
            "user_dialog_claim".into(),
            crate::metadata::object([
                ("sessionKey", JsValue::String(session.clone())),
                ("requestId", "r5-claim-null-lease".into()),
                ("leaseId", JsValue::Null),
            ]),
        )
        .await
        .expect_err("#1309 r5 claim leaseId null is rejected");
    negative_assert_eq!(
        error.code,
        "INVALID_USER_DIALOG_CLAIM",
        "#1309 r5 claim leaseId null error code"
    );
    negative_assert_eq!(
        message(&error),
        "leaseId must be a non-empty string when provided.",
        "#1309 r5 claim leaseId null error message"
    );

    // respond leaseId: null -> rejected (InProcessGateway.ts:2655).
    register_null_tri_state_dialog(
        &requests,
        &session,
        "r5-respond-null-lease",
        true,
        &resolved,
    );
    let error = requests
        .user_dialog_request(
            "user_dialog_respond".into(),
            crate::metadata::object([
                ("sessionKey", JsValue::String(session.clone())),
                ("requestId", "r5-respond-null-lease".into()),
                ("leaseId", JsValue::Null),
                (
                    "result",
                    crate::metadata::object([("behavior", "cancelled".into())]),
                ),
            ]),
        )
        .await
        .expect_err("#1309 r5 respond leaseId null is rejected");
    negative_assert_eq!(
        error.code,
        "INVALID_USER_DIALOG_RESPONSE",
        "#1309 r5 respond leaseId null error code"
    );
    negative_assert_eq!(
        message(&error),
        "leaseId must be a non-empty string when provided.",
        "#1309 r5 respond leaseId null error message"
    );

    // cancelled reason: null -> rejected (InProcessGateway.ts:2652); only a
    // missing/undefined reason is omitted.
    register_null_tri_state_dialog(&requests, &session, "r5-null-reason", true, &resolved);
    let error = requests
        .user_dialog_request(
            "user_dialog_respond".into(),
            crate::metadata::object([
                ("sessionKey", JsValue::String(session.clone())),
                ("requestId", "r5-null-reason".into()),
                (
                    "result",
                    crate::metadata::object([
                        ("behavior", "cancelled".into()),
                        ("reason", JsValue::Null),
                    ]),
                ),
            ]),
        )
        .await
        .expect_err("#1309 r5 cancelled reason null is rejected");
    negative_assert_eq!(
        error.code,
        "INVALID_USER_DIALOG_RESPONSE",
        "#1309 r5 cancelled reason null error code"
    );
    negative_assert_eq!(
        message(&error),
        "cancelled user dialog reason must be a string.",
        "#1309 r5 cancelled reason null error message"
    );

    // answered value missing -> still the "requires a value" guard
    // (InProcessGateway.ts:2649) — the null cases below must NOT hit it.
    register_null_tri_state_dialog(&requests, &session, "r5-missing-value", true, &resolved);
    let error = requests
        .user_dialog_request(
            "user_dialog_respond".into(),
            crate::metadata::object([
                ("sessionKey", JsValue::String(session.clone())),
                ("requestId", "r5-missing-value".into()),
                (
                    "result",
                    crate::metadata::object([("behavior", "answered".into())]),
                ),
            ]),
        )
        .await
        .expect_err("#1309 r5 answered missing value is rejected");
    negative_assert_eq!(
        message(&error),
        "answered user dialog results require a value.",
        "#1309 r5 answered missing value error message"
    );

    // answered value: null + accepts(null) -> delivered, and the resolved
    // answer carries the null value (TS passes it to entry.accepts,
    // InProcessGateway.ts:2674).
    register_null_tri_state_dialog(
        &requests,
        &session,
        "r5-null-value-accepts",
        true,
        &resolved,
    );
    let result = requests
        .user_dialog_request(
            "user_dialog_respond".into(),
            crate::metadata::object([
                ("sessionKey", JsValue::String(session.clone())),
                ("requestId", "r5-null-value-accepts".into()),
                (
                    "result",
                    crate::metadata::object([
                        ("behavior", "answered".into()),
                        ("value", JsValue::Null),
                    ]),
                ),
            ]),
        )
        .await
        .expect("#1309 r5 answered null value is delivered when accepted");
    negative_assert_eq!(
        result.get("delivered"),
        Some(&JsValue::Bool(true)),
        "#1309 r5 answered null value delivered"
    );
    negative_assert_eq!(
        resolved.lock().unwrap().as_slice(),
        [crate::user_dialog::DialogAnswer::Answered(JsValue::Null)],
        "#1309 r5 answered null value reaches the resolver as null"
    );

    // answered value: null + rejects(null) -> the dialog contract error, not
    // the missing-value guard.
    register_null_tri_state_dialog(
        &requests,
        &session,
        "r5-null-value-rejects",
        false,
        &resolved,
    );
    let error = requests
        .user_dialog_request(
            "user_dialog_respond".into(),
            crate::metadata::object([
                ("sessionKey", JsValue::String(session.clone())),
                ("requestId", "r5-null-value-rejects".into()),
                (
                    "result",
                    crate::metadata::object([
                        ("behavior", "answered".into()),
                        ("value", JsValue::Null),
                    ]),
                ),
            ]),
        )
        .await
        .expect_err("#1309 r5 answered null value against a rejecting contract");
    negative_assert_eq!(
        error.code,
        "INVALID_USER_DIALOG_RESPONSE",
        "#1309 r5 answered null value contract error code"
    );
    negative_assert_eq!(
        message(&error),
        "answered question dialog result does not match the pending dialog contract.",
        "#1309 r5 answered null value contract error message"
    );

    requests
        .owner
        .shutdown()
        .await
        .expect("#1309 r5 fixture owner shutdown");
    requests
        .project
        .dispose()
        .await
        .expect("#1309 r5 fixture project shutdown");
}

// #1309 fix-r6 (gate r5 P1): the bus sees the TRIMMED leaseId, matching TS
// `input.leaseId?.trim()` / `.trim()` at the three call sites (claim
// InProcessGateway.ts:2754, respond :2683, release :2769). A padded
// " <uuid> " must renew the same lease, deliver a response and release,
// not collide with itself (the untrimmed old behavior returned Busy /
// lease_required / released:false).
#[tokio::test]
async fn user_dialog_padded_lease_id_matches_trimmed() {
    let (_scratch, mut requests) = std::panic::catch_unwind(super::super::management_tests::setup)
        .expect("#1309 fix-r6 fixture setup");
    let session: JsString = "p2a-r6-session".into();
    let resolved: std::sync::Arc<std::sync::Mutex<Vec<crate::user_dialog::DialogAnswer>>> =
        std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    requests.user_dialog_bus = std::sync::Arc::new(std::sync::Mutex::new(
        crate::user_dialog::GatewayUserDialogBus::new(
            crate::user_dialog::DialogRuntime::default(),
            Some(Box::new(|_event| Ok(()))),
        ),
    ));
    register_null_tri_state_dialog(&requests, &session, "r6-lease", true, &resolved);

    // Initial claim without a leaseId -> a fresh lease.
    let result = requests
        .user_dialog_request(
            "user_dialog_claim".into(),
            crate::metadata::object([
                ("sessionKey", JsValue::String(session.clone())),
                ("requestId", "r6-lease".into()),
            ]),
        )
        .await
        .expect("#1309 r6 initial claim succeeds");
    negative_assert_eq!(
        result.get("claimed"),
        Some(&JsValue::Bool(true)),
        "#1309 r6 initial claim"
    );
    let lease = result
        .get("leaseId")
        .and_then(JsValue::as_string)
        .expect("#1309 r6 lease id present");
    let lease_text = lease
        .to_utf8()
        .expect("#1309 r6 lease id UTF-8")
        .to_string();

    // Renew with a padded leaseId -> renews the SAME lease (TS passes the
    // trimmed id into the bus, InProcessGateway.ts:2754).
    let result = requests
        .user_dialog_request(
            "user_dialog_claim".into(),
            crate::metadata::object([
                ("sessionKey", JsValue::String(session.clone())),
                ("requestId", "r6-lease".into()),
                (
                    "leaseId",
                    JsValue::String(format!("  {lease_text}  ").into()),
                ),
            ]),
        )
        .await
        .expect("#1309 r6 padded lease renew succeeds");
    negative_assert_eq!(
        result.get("claimed"),
        Some(&JsValue::Bool(true)),
        "#1309 r6 padded renew claims"
    );
    negative_assert_eq!(
        result.get("leaseId"),
        Some(&JsValue::String(lease.clone())),
        "#1309 r6 padded renew keeps the lease id"
    );

    // Release with a padded leaseId -> released (TS :2769 passes trimmed).
    let result = requests
        .user_dialog_request(
            "user_dialog_release".into(),
            crate::metadata::object([
                ("sessionKey", JsValue::String(session.clone())),
                ("requestId", "r6-lease".into()),
                (
                    "leaseId",
                    JsValue::String(format!("\t{lease_text}\n").into()),
                ),
            ]),
        )
        .await
        .expect("#1309 r6 release succeeds");
    negative_assert_eq!(
        result.get("released"),
        Some(&JsValue::Bool(true)),
        "#1309 r6 padded release releases"
    );

    // Respond with a padded leaseId against a live claim -> delivered (TS
    // :2683 passes the trimmed id; untrimmed would hit lease_required).
    register_null_tri_state_dialog(&requests, &session, "r6-respond-lease", true, &resolved);
    let result = requests
        .user_dialog_request(
            "user_dialog_claim".into(),
            crate::metadata::object([
                ("sessionKey", JsValue::String(session.clone())),
                ("requestId", "r6-respond-lease".into()),
            ]),
        )
        .await
        .expect("#1309 r6 respond-path claim succeeds");
    let lease = result
        .get("leaseId")
        .and_then(JsValue::as_string)
        .expect("#1309 r6 respond-path lease id present");
    let lease_text = lease
        .to_utf8()
        .expect("#1309 r6 lease id UTF-8")
        .to_string();
    let result = requests
        .user_dialog_request(
            "user_dialog_respond".into(),
            crate::metadata::object([
                ("sessionKey", JsValue::String(session.clone())),
                ("requestId", "r6-respond-lease".into()),
                ("leaseId", JsValue::String(format!(" {lease_text} ").into())),
                (
                    "result",
                    crate::metadata::object([("behavior", "cancelled".into())]),
                ),
            ]),
        )
        .await;
    // #1309 fix-r7: a de-trim mutation on the respond call site surfaces as
    // USER_DIALOG_LEASE_REQUIRED from consume_for_response. Routing the
    // failure through the receipt-aware expect — labeled like the bound target
    // assertion below — keeps the mutation attributable to this test's target
    // assertion instead of an unattributable setup panic (nonce-receipt
    // contract, tests/common/negative_receipt.rs).
    let result =
        crate::test_negative_receipt::expect_ok(result, "#1309 r6 padded respond delivered");
    negative_assert_eq!(
        result.get("delivered"),
        Some(&JsValue::Bool(true)),
        "#1309 r6 padded respond delivered"
    );

    requests
        .owner
        .shutdown()
        .await
        .expect("#1309 r6 fixture owner shutdown");
    requests
        .project
        .dispose()
        .await
        .expect("#1309 r6 fixture project shutdown");
}

// #1309 fix-r7 (codex P1): release has NO typeof short-circuit on leaseId
// (InProcessGateway.ts:2766-2769, unlike claim :2738 / respond :2655), so a
// non-string leaseId surfaces on the wire as the generic transport failure,
// not the domain code. Raw TS wire probe:
// main-repo _tmp/20261105-issue-1309-fix-r7/ts-probe/ts-probe-output.txt.
#[tokio::test]
async fn user_dialog_release_nonstring_lease_id_matches_typescript_wire() {
    let (_scratch, mut requests) = std::panic::catch_unwind(super::super::management_tests::setup)
        .expect("#1309 fix-r7 fixture setup");
    let session: JsString = "p2a-r7-session".into();
    requests.user_dialog_bus = std::sync::Arc::new(std::sync::Mutex::new(
        crate::user_dialog::GatewayUserDialogBus::new(
            crate::user_dialog::DialogRuntime::default(),
            Some(Box::new(|_event| Ok(()))),
        ),
    ));
    for (label, lease_id) in [
        ("number", JsValue::Number(5.0)),
        (
            "object",
            crate::metadata::object([("uuid", "lease".into())]),
        ),
        ("array", JsValue::Array(vec![JsValue::Number(1.0)])),
        ("boolean", JsValue::Bool(true)),
    ] {
        let error = requests
            .user_dialog_request(
                "user_dialog_release".into(),
                crate::metadata::object([
                    ("sessionKey", JsValue::String(session.clone())),
                    ("requestId", "r7-release".into()),
                    ("leaseId", lease_id),
                ]),
            )
            .await
            .expect_err("#1309 r7 non-string leaseId is a transport failure");
        negative_assert_eq!(
            error.code,
            "gateway_request_failed",
            "#1309 r7 release {label} leaseId wire code"
        );
        negative_assert_eq!(
            message(&error),
            "input.leaseId?.trim is not a function",
            "#1309 r7 release {label} leaseId wire message"
        );
    }
    // Probe baselines: null, empty, and missing leaseIds keep the domain code.
    for (label, params) in [
        (
            "null",
            crate::metadata::object([
                ("sessionKey", JsValue::String(session.clone())),
                ("requestId", "r7-release".into()),
                ("leaseId", JsValue::Null),
            ]),
        ),
        (
            "empty",
            crate::metadata::object([
                ("sessionKey", JsValue::String(session.clone())),
                ("requestId", "r7-release".into()),
                ("leaseId", "".into()),
            ]),
        ),
        (
            "missing",
            crate::metadata::object([
                ("sessionKey", JsValue::String(session.clone())),
                ("requestId", "r7-release".into()),
            ]),
        ),
    ] {
        let error = requests
            .user_dialog_request("user_dialog_release".into(), params)
            .await
            .expect_err("#1309 r7 nullish or empty leaseId keeps the domain rejection");
        negative_assert_eq!(
            error.code,
            "INVALID_USER_DIALOG_RELEASE",
            "#1309 r7 release {label} leaseId keeps the domain code"
        );
    }
    requests
        .owner
        .shutdown()
        .await
        .expect("#1309 fix-r7 fixture owner shutdown");
    requests
        .project
        .dispose()
        .await
        .expect("#1309 fix-r7 fixture project shutdown");
}

// #1309 fix-r7 (gate sibling#2): the permission family had only negative
// controls. Positive control for the listPermissions snapshot projection
// (InProcessGateway.ts:2784-2789) and the grantSessionPermission success
// result.
#[tokio::test]
async fn permission_list_projection_and_grant_succeed() {
    let (_scratch, requests) = std::panic::catch_unwind(super::super::management_tests::setup)
        .expect("#1309 permission positive fixture setup");
    let session: JsString = "p2a-r7-perm".into();

    // Two pending permission requests: one with a payload, one without.
    for (request_id, payload) in [
        (
            "perm-with-payload",
            Some(crate::metadata::object([("command", "echo hi".into())])),
        ),
        ("perm-no-payload", None),
    ] {
        requests
            .interactions
            .permission_bus()
            .register(
                &session,
                crate::runtime::interaction::PendingEntry {
                    request_id: request_id.into(),
                    tool_call_id: "call-9".into(),
                    tool_name: "Bash".into(),
                    resolve: std::sync::Arc::new(|_decision: JsValue| {}),
                    reject: std::sync::Arc::new(|_reason: JsString| {}),
                },
                payload,
            )
            .expect("#1309 permission positive registration");
    }

    let result = requests
        .permission_request(
            "permission_list".into(),
            crate::metadata::object([("sessionKey", JsValue::String(session.clone()))]),
        )
        .await
        .expect("#1309 permission_list succeeds");
    let entries = result.get("requests").and_then(|value| match value {
        JsValue::Array(entries) => Some(entries),
        _ => None,
    });
    negative_assert!(entries.is_some(), "#1309 permission_list projects an array");
    let entries = entries.expect("#1309 permission_list requests array");
    negative_assert_eq!(
        entries.len(),
        2,
        "#1309 permission_list projects both entries"
    );
    negative_assert_eq!(
        entries[0].get("requestId"),
        Some(&JsValue::String("perm-with-payload".into())),
        "#1309 permission_list requestId"
    );
    negative_assert_eq!(
        entries[0].get("toolCallId"),
        Some(&JsValue::String("call-9".into())),
        "#1309 permission_list toolCallId"
    );
    negative_assert_eq!(
        entries[0].get("toolName"),
        Some(&JsValue::String("Bash".into())),
        "#1309 permission_list toolName"
    );
    negative_assert_eq!(
        entries[0]
            .get("payload")
            .and_then(|payload| payload.get("command")),
        Some(&JsValue::String("echo hi".into())),
        "#1309 permission_list forwards the payload"
    );
    negative_assert!(
        entries[1].get("payload").is_none(),
        "#1309 permission_list omits an absent payload"
    );

    // Snapshot rows always carry the three ids; the `?? ""` fallback must hold
    // for a row that lacks them, and a present null payload is forwarded
    // (TS `request.payload !== undefined`, InProcessGateway.ts:2789).
    let raw = pilotdeck_protocol::parse_json(r#"{"requestId":"r","payload":null}"#)
        .expect("#1309 projection fixture JSON");
    let projected = super::permission::project_permission_entry(&raw);
    negative_assert_eq!(
        projected.get("requestId"),
        Some(&JsValue::String("r".into())),
        "#1309 projection keeps a present requestId"
    );
    negative_assert_eq!(
        projected.get("toolCallId"),
        Some(&JsValue::String("".into())),
        "#1309 projection falls back to empty toolCallId"
    );
    negative_assert_eq!(
        projected.get("toolName"),
        Some(&JsValue::String("".into())),
        "#1309 projection falls back to empty toolName"
    );
    negative_assert_eq!(
        projected.get("payload"),
        Some(&JsValue::Null),
        "#1309 projection forwards a null payload"
    );
    let raw = pilotdeck_protocol::parse_json(r#"{"requestId":"r"}"#)
        .expect("#1309 projection fixture JSON");
    let projected = super::permission::project_permission_entry(&raw);
    negative_assert!(
        projected.get("payload").is_none(),
        "#1309 projection omits an undefined payload"
    );

    // grant_session_permission success: the granted rule echoes its entry.
    let result = requests
        .permission_request(
            "grant_session_permission".into(),
            crate::metadata::object([
                ("sessionKey", JsValue::String(session.clone())),
                ("entry", "Bash(echo hi)".into()),
            ]),
        )
        .await
        .expect("#1309 grant succeeds");
    negative_assert_eq!(
        result.get("granted"),
        Some(&JsValue::Bool(true)),
        "#1309 grant reports granted"
    );
    negative_assert_eq!(
        result.get("entry"),
        Some(&JsValue::String("Bash(echo hi)".into())),
        "#1309 grant echoes the entry"
    );
    requests
        .owner
        .shutdown()
        .await
        .expect("#1309 permission positive fixture owner shutdown");
    requests
        .project
        .dispose()
        .await
        .expect("#1309 permission positive fixture project shutdown");
}
