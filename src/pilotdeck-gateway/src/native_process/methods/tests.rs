use super::*;
use crate::negative_assert_eq;
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
