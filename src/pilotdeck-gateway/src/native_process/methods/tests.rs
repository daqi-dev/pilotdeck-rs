use super::*;
use crate::{negative_assert, negative_assert_eq};
use std::collections::BTreeSet;
use std::sync::Arc;

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

/// A recorded mailbox discard: `(coerced item id, reason)`.
type RecordedDiscard = (
    pilotdeck_protocol::JsString,
    Option<pilotdeck_protocol::JsString>,
);

/// Captures the durable `claim` / `discard` mutations the mailbox records
/// through the injected `MutationRecorder` (A7.2-STEER-INPUT-001 差异 6/7):
/// claims keep the mutated id text, discards keep `(id, reason)`.
#[derive(Default)]
struct RecordedClaims {
    claims: std::sync::Mutex<Vec<pilotdeck_protocol::JsString>>,
    discards: std::sync::Mutex<Vec<RecordedDiscard>>,
}
impl crate::runtime::steer::MutationRecorder for RecordedClaims {
    fn record(
        &self,
        _turn_id: pilotdeck_protocol::JsString,
        mutation: pilotdeck_protocol::JsObject,
    ) -> crate::host::HostFuture<()> {
        let kind = mutation.get("mutation").and_then(JsValue::as_string);
        if let Some(JsValue::String(id)) = mutation.get("itemId") {
            if kind == Some(&pilotdeck_protocol::JsString::from("claim")) {
                self.claims.lock().unwrap().push(id.clone());
            } else if kind == Some(&pilotdeck_protocol::JsString::from("discard")) {
                let reason = match mutation.get("reason") {
                    Some(JsValue::String(reason)) => Some(reason.clone()),
                    _ => None,
                };
                self.discards.lock().unwrap().push((id.clone(), reason));
            }
        }
        Box::pin(async { Ok::<(), crate::host::HostError>(()) })
    }
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
        // #1316 A7.8-pr1: the C4 interaction round-trips route to the
        // injected GatewayInteractionCoordinator instead of the metadata
        // fallback (submit_turn/abort_turn stay connection-owned).
        "reconnect_interaction",
        "elicitation_respond",
        "permission_decide",
        // #1313 r1: always-on is wired back in (2026-11-08 lift of the
        // 2026-11-05 #1475 deferral); the three methods answer the TS
        // not-configured result envelopes.
        "always_on_apply",
        "always_on_abort",
        "always_on_rerun_plan",
        // #1328 U12: connection-level wire method, registered in its own
        // family (not an advertised capability — TS keeps it out of
        // describeServer's capabilities list too).
        "prepare_weixin_login",
        // #1320 U7 global settings methods
        "apply_flag_settings",
        "update_settings",
        "resolve_settings",
        "reload_output_styles",
        "reload_config",
        "reload_extensions",
        // #1326 A8 U8: the six cron control names, one registration
        // (methods/cron.rs) whose method table is `runtime::cron`'s own
        // `CRON_METHODS`.
        "cron_create",
        "cron_list",
        "cron_update",
        "cron_delete",
        "cron_stop",
        "cron_run_now",
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

// #1316 A7.8-pr1: reconcile the assembled session domain with the delivery
// manifest. Every A7 delivery-package method must be dispatched — by a
// registered family, or by the connection-owned submit/abort paths — with no
// duplicates and no silent omissions. The three always-on methods stay in the
// manifest and in the registry for their TS not-configured result envelopes
// (#1313; the 2026-11-05 CAPABILITY_UNAVAILABLE rescope was lifted on
// 2026-11-08).
#[test]
fn session_domain_reconciles_with_coverage_manifest() {
    // A plain string literal keeps the #1166 build-input scanner working.
    let manifest: serde_json::Value = serde_json::from_str(include_str!(
        "../../../../../specs/gateway-coverage/coverage-manifest.json"
    ))
    .expect("#1316 coverage manifest JSON");
    let entries = manifest["methods"]
        .as_array()
        .expect("#1316 manifest method entries");
    let mut a7: Vec<&str> = entries
        .iter()
        .filter(|entry| {
            entry["delivery_package"]
                .as_str()
                .expect("#1316 delivery package")
                .starts_with("A7")
        })
        .map(|entry| entry["method"].as_str().expect("#1316 method name"))
        .collect();
    a7.sort_unstable();
    negative_assert_eq!(a7.len(), 52, "#1316 A7 delivery package method count");
    negative_assert_eq!(
        a7.iter().collect::<BTreeSet<_>>().len(),
        52,
        "#1316 A7 methods contain no duplicate"
    );
    // submit_turn (submit path) and abort_turn are dispatched by the
    // connection layer (runtime/turn/gateway.rs), not the method registry.
    let connection_owned = ["submit_turn", "abort_turn"];
    let registered: BTreeSet<_> = registered_methods().collect();
    for method in &a7 {
        negative_assert!(
            registered.contains(method) || connection_owned.contains(method),
            "#1316 A7 method {method} is dispatched"
        );
    }
    for method in ["always_on_apply", "always_on_abort", "always_on_rerun_plan"] {
        negative_assert!(
            a7.contains(&method),
            "#1313 always-on method {method} stays in the manifest"
        );
    }
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
        // #1328: prepare_weixin_login is the second BeforeAdmission family
        // (TS dispatches it at the connection switch), so the admission-phase
        // expectation covers both pre-admission families.
        let expected_before_admission = expected || method == "prepare_weixin_login";
        negative_assert_eq!(
            before_admission(&requests, method, &params).await.is_some(),
            expected_before_admission,
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
async fn steer_turn_merges_attachments_into_content() {
    // #1444: TS steerTurn merges `attachments` into the content array via
    // prepareAttachmentTurn (InProcessGateway.ts:1864-1870) and never reads
    // `uploadedAttachments`. Without a live turn every attachment-shaped
    // input takes the ordinary no-active-turn path — the admission gates run
    // before the attachment prep (probe
    // `_tmp/20261026-issue-1444/ts-probe/output.txt` a14).
    use crate::runtime::turn::gateway::GatewayRequests;

    let (_scratch, requests) = std::panic::catch_unwind(super::super::management_tests::setup)
        .expect("#1307 registry fixture setup");
    let text_attachment = || {
        JsValue::Array(vec![crate::metadata::object([
            ("type", "text".into()),
            ("content", "attachment body".into()),
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
    // No attachments, an explicit empty array, a non-empty `attachments`, and
    // a non-empty `uploadedAttachments` (never read by TS steerTurn) with no
    // active turn: all report the ordinary no-active-turn rejection in TS
    // order (admission gates before attachments).
    for params in [
        steer_params(None),
        steer_params(Some(("attachments", JsValue::Array(vec![])))),
        steer_params(Some(("attachments", text_attachment()))),
        steer_params(Some(("uploadedAttachments", text_attachment()))),
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
    // With a live turn a non-empty `attachments` composes: the message text
    // block plus the attachment's own text block (probe a13).
    let result = requests
        .request(
            "steer_turn".into(),
            steer_params(Some(("attachments", text_attachment()))),
        )
        .await
        .expect("#1444 attachments merge stays Ok with a live turn");
    negative_assert_eq!(
        result.get("accepted"),
        Some(&JsValue::Bool(true)),
        "#1444 attachments do not block the steer"
    );
    let drained = steer
        .mailbox_for_test(&"s".into())
        .expect("#1307 session mailbox")
        .drain(&"t".into());
    negative_assert_eq!(
        drained.len(),
        1,
        "#1444 exactly the composed steer message is queued"
    );
    let delivered = &drained[0];
    negative_assert_eq!(
        delivered.message.get("content"),
        Some(&JsValue::Array(vec![
            crate::metadata::object([("type", "text".into()), ("text", "hello".into()),]),
            crate::metadata::object([("type", "text".into()), ("text", "attachment body".into()),]),
        ])),
        "#1444 attachments merge into the content array (probe a13)"
    );
    negative_assert_eq!(
        delivered.allowed_read_files,
        Some(Vec::new()),
        "#1444 an inline text attachment registers no read files"
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
    // TS steerTurn order (probe a7-a9/a14/a15): the two activeTurnRunId
    // branches come first (InProcessGateway.ts:1860-1862), the composer's
    // `for...of` boundary TypeError for a truthy non-iterable `attachments`
    // fires immediately after (:1864-1870), and the session/run resource
    // gates only run inside router.steer (:1878-1883 → SessionRouter.ts:
    // 307-314, AgentSession.ts:290-297). The boundary TypeError therefore
    // fires once the admission branches pass — even when the steer-side
    // resource gate would still have reported no_active_turn — while
    // turn_mismatch still wins outright.
    use crate::runtime::turn::gateway::GatewayRequests;

    let (_scratch, requests) = std::panic::catch_unwind(super::super::management_tests::setup)
        .expect("#1307 registry fixture setup");
    let bad = || JsValue::Number(1.0);
    let good = || {
        JsValue::Array(vec![crate::metadata::object([
            ("type", "text".into()),
            ("content", "attachment body".into()),
        ])])
    };
    let steer_params = |run_id: &str, attachments: Option<JsValue>| {
        let fields: Vec<(&'static str, JsValue)> = [
            ("sessionKey", "s".into()),
            ("runId", run_id.into()),
            ("itemId", "i".into()),
            ("message", "hello".into()),
        ]
        .into_iter()
        .chain(attachments.map(|value| ("attachments", value)))
        .collect();
        crate::metadata::object(fields)
    };
    // No admission slot at all: the activeTurnRunId miss wins over the
    // boundary TypeError (probe a14).
    let result = requests
        .request("steer_turn".into(), steer_params("r", Some(bad())))
        .await
        .expect("#1444 no-active-turn branch stays Ok");
    negative_assert_eq!(
        result.get("reason").and_then(JsValue::as_string),
        Some(&pilotdeck_protocol::JsString::from("no_active_turn")),
        "#1444 no_active_turn precedes the attachments boundary"
    );
    // turn_mismatch precedes the TypeError: the admission branch fires first
    // (probe a15).
    let _reservation = requests
        .owner
        .begin_turn(&"s".into(), &"r".into())
        .expect("#1307 admission slot");
    let result = requests
        .request("steer_turn".into(), steer_params("x", Some(bad())))
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
    // Admission matched but no steer run staged: TS still prepares the
    // attachments before router.steer reports no_active_turn, so the
    // boundary TypeError fires here instead.
    let error = requests
        .request("steer_turn".into(), steer_params("r", Some(bad())))
        .await
        .expect_err("#1444 attachments boundary precedes the resource gate");
    negative_assert_eq!(
        error.message,
        pilotdeck_protocol::JsString::from("attachments is not iterable"),
        "#1444 the boundary error keeps the JS TypeError text"
    );
    negative_assert_eq!(
        error.code,
        "gateway_request_failed",
        "#1444 the boundary error rides the request error code"
    );
    // Same when a steer run exists but its run ID disagrees with the
    // admission slot: that resource gate only runs after attachment prep.
    let steer = steer_resource(&requests);
    let _turn = start_steer_run(&steer, "s", "t", "other");
    let error = requests
        .request("steer_turn".into(), steer_params("r", Some(bad())))
        .await
        .expect_err("#1444 attachments boundary precedes the slot-mismatch gate");
    negative_assert_eq!(
        error.message,
        pilotdeck_protocol::JsString::from("attachments is not iterable"),
        "#1444 the boundary error precedes the slot-mismatch gate"
    );
    // The success side proves the compose precedes the gate the same way: a
    // valid attachment payload with no matching steer run still reports the
    // resource gate's no_active_turn instead of an error.
    let result = requests
        .request("steer_turn".into(), steer_params("r", Some(good())))
        .await
        .expect("#1444 composed attachments reach the resource gate");
    negative_assert_eq!(
        result.get("reason").and_then(JsValue::as_string),
        Some(&pilotdeck_protocol::JsString::from("no_active_turn")),
        "#1444 a composed steer still reports the resource gate"
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
async fn steer_turn_non_array_attachments_follow_the_composer_boundary() {
    // #1444: the non-array spellings keep the composer's own boundary
    // (GatewayAttachmentTurnComposer.ts:97/:177/:199) — each pass opens with
    // `!attachments || attachments.length === 0` and then iterates, so every
    // falsy non-nullish value short-circuits to the plain-text turn while a
    // truthy non-iterable throws (probe a4-a10). Unlike the submit family's
    // spread, `0`/`false` do NOT throw here.
    use crate::runtime::turn::gateway::GatewayRequests;

    let (_scratch, requests) = std::panic::catch_unwind(super::super::management_tests::setup)
        .expect("#1307 registry fixture setup");
    let _reservation = requests
        .owner
        .begin_turn(&"s".into(), &"r".into())
        .expect("#1307 admission slot");
    let steer = steer_resource(&requests);
    let _turn = start_steer_run(&steer, "s", "t", "r");
    let plain_content = JsValue::Array(vec![crate::metadata::object([
        ("type", "text".into()),
        ("text", "hello".into()),
    ])]);
    let mut accepted = 0;
    for (index, (reading, value)) in [
        ("empty string", JsValue::from("")),
        ("zero", JsValue::Number(0.0)),
        ("NaN", JsValue::Number(f64::NAN)),
        ("false", JsValue::Bool(false)),
        ("non-empty string", JsValue::from("ab")),
        ("null", JsValue::Null),
    ]
    .into_iter()
    .enumerate()
    {
        // Distinct item ids: the mailbox's durable claim gate discards a
        // repeat id within one turn (A7.2-STEER-INPUT-001 差异 6).
        let item_id = format!("i{index}");
        let result = requests
            .request(
                "steer_turn".into(),
                crate::metadata::object([
                    ("sessionKey", "s".into()),
                    ("runId", "r".into()),
                    ("itemId", JsValue::from(item_id.as_str())),
                    ("message", "hello".into()),
                    ("attachments", value),
                ]),
            )
            .await
            .unwrap_or_else(|_| panic!("#1444 {reading} attachments must stay Ok"));
        negative_assert_eq!(
            result.get("accepted"),
            Some(&JsValue::Bool(true)),
            "#1444 {reading} attachments keep the plain turn"
        );
        accepted += 1;
    }
    // Every accepted cell delivered the plain no-attachment message.
    let drained = steer
        .mailbox_for_test(&"s".into())
        .expect("#1307 session mailbox")
        .drain(&"t".into());
    negative_assert_eq!(
        drained.len(),
        accepted,
        "#1444 each plain spelling enqueues exactly one steer"
    );
    for delivered in &drained {
        negative_assert_eq!(
            delivered.message.get("content"),
            Some(&plain_content),
            "#1444 falsy/non-nullish attachments compose the plain text turn"
        );
        negative_assert_eq!(
            delivered.allowed_read_files,
            Some(Vec::new()),
            "#1444 non-array attachments register no read files"
        );
    }
    // Truthy non-iterables throw the composer's boundary TypeError
    // (probe a7/a8/a9) — after the admission gates, before the resource
    // gates (see steer_turn_attachments_sit_between_admission_and_resource_
    // gates).
    for (reading, value) in [
        ("one", JsValue::Number(1.0)),
        ("true", JsValue::Bool(true)),
        ("object", crate::metadata::object([])),
    ] {
        let error = requests
            .request(
                "steer_turn".into(),
                crate::metadata::object([
                    ("sessionKey", "s".into()),
                    ("runId", "r".into()),
                    ("itemId", "i".into()),
                    ("message", "hello".into()),
                    ("attachments", value),
                ]),
            )
            .await
            .expect_err("#1444 truthy non-iterable attachments must throw");
        negative_assert_eq!(
            error.message,
            pilotdeck_protocol::JsString::from("attachments is not iterable"),
            "#1444 {reading} keeps the JS TypeError text"
        );
        negative_assert_eq!(
            error.code,
            "gateway_request_failed",
            "#1444 {reading} rides the request error code"
        );
    }
    // Nothing further was queued by the error cells.
    let drained = steer
        .mailbox_for_test(&"s".into())
        .expect("#1307 session mailbox")
        .drain(&"t".into());
    negative_assert_eq!(
        drained.len(),
        0,
        "#1444 the boundary TypeError enqueues nothing"
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
async fn steer_turn_object_attachments_follow_the_length_zero_front_door() {
    // #1444 fix-r1 (review MM boundary #1): the composer's front door
    // short-circuits on the raw value's `length === 0`
    // (GatewayAttachmentTurnComposer.ts:97/:177/:199), not just on
    // falsiness: a truthy object carrying the number 0 folds to the plain
    // turn like the falsy spellings (`{length:0}` b1 and `{0:{…},length:0}`
    // b2 — fix-r1 probe
    // `_tmp/20261026-issue-1444/fix-r1/ts-probe/output.txt`), while every
    // other object still reaches the `for...of` TypeError (`{length:2}` b3,
    // `{length:"0"}` b4 — the check is `=== 0` on the raw value, so a
    // nonzero number or a string "0" does not fold).
    use crate::runtime::turn::gateway::GatewayRequests;

    let (_scratch, requests) = std::panic::catch_unwind(super::super::management_tests::setup)
        .expect("#1307 registry fixture setup");
    let _reservation = requests
        .owner
        .begin_turn(&"s".into(), &"r".into())
        .expect("#1307 admission slot");
    let steer = steer_resource(&requests);
    let _turn = start_steer_run(&steer, "s", "t", "r");
    let plain_content = JsValue::Array(vec![crate::metadata::object([
        ("type", "text".into()),
        ("text", "hello".into()),
    ])]);
    // Distinct item ids: the mailbox's durable claim gate discards a repeat
    // id within one turn (A7.2-STEER-INPUT-001 差异 6).
    for (index, (reading, value)) in [
        (
            "numeric zero length",
            crate::metadata::object([("length", JsValue::Number(0.0))]),
        ),
        (
            "array-like with zero length",
            crate::metadata::object([
                (
                    "0",
                    crate::metadata::object([
                        ("type", "text".into()),
                        ("content", "attachment body".into()),
                    ]),
                ),
                ("length", JsValue::Number(0.0)),
            ]),
        ),
    ]
    .into_iter()
    .enumerate()
    {
        let item_id = format!("i{index}");
        let result = requests
            .request(
                "steer_turn".into(),
                crate::metadata::object([
                    ("sessionKey", "s".into()),
                    ("runId", "r".into()),
                    ("itemId", JsValue::from(item_id.as_str())),
                    ("message", "hello".into()),
                    ("attachments", value),
                ]),
            )
            .await
            .unwrap_or_else(|_| panic!("#1444 {reading} attachments must stay Ok"));
        negative_assert_eq!(
            result.get("accepted"),
            Some(&JsValue::Bool(true)),
            "#1444 {reading} attachments keep the plain turn"
        );
    }
    let drained = steer
        .mailbox_for_test(&"s".into())
        .expect("#1307 session mailbox")
        .drain(&"t".into());
    negative_assert_eq!(
        drained.len(),
        2,
        "#1444 each zero-length object spelling enqueues exactly one steer"
    );
    for delivered in &drained {
        negative_assert_eq!(
            delivered.message.get("content"),
            Some(&plain_content),
            "#1444 zero-length objects compose the plain text turn"
        );
        negative_assert_eq!(
            delivered.allowed_read_files,
            Some(Vec::new()),
            "#1444 zero-length objects register no read files"
        );
    }
    // Every other object still throws (b3/b4): the front door is `=== 0` on
    // the raw value, so a nonzero number or the string "0" does not fold.
    for (reading, value) in [
        (
            "numeric length two",
            crate::metadata::object([("length", JsValue::Number(2.0))]),
        ),
        (
            "string zero length",
            crate::metadata::object([("length", JsValue::from("0"))]),
        ),
    ] {
        let error = requests
            .request(
                "steer_turn".into(),
                crate::metadata::object([
                    ("sessionKey", "s".into()),
                    ("runId", "r".into()),
                    ("itemId", "i".into()),
                    ("message", "hello".into()),
                    ("attachments", value),
                ]),
            )
            .await
            .expect_err(&format!(
                "#1444 {reading} must reach the for...of TypeError"
            ));
        negative_assert_eq!(
            error.message,
            pilotdeck_protocol::JsString::from("attachments is not iterable"),
            "#1444 {reading} keeps the JS TypeError text"
        );
        negative_assert_eq!(
            error.code,
            "gateway_request_failed",
            "#1444 {reading} rides the request error code"
        );
    }
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
async fn steer_turn_message_cells_keep_the_raw_block_and_the_submit_guard() {
    // #1444: without attachments the text block embeds the raw wire message
    // verbatim (probe a11/a16/a17). With a non-empty `attachments` the steer
    // reuses the submit-lane prepare, whose string-message guard fires
    // before the composer — TS instead drops the raw message block and
    // composes the attachment turn (probe a12): ledger
    // A7.2-STEER-ATTACH-001-R2 差异 1.
    use crate::runtime::turn::gateway::GatewayRequests;

    let (_scratch, requests) = std::panic::catch_unwind(super::super::management_tests::setup)
        .expect("#1307 registry fixture setup");
    let _reservation = requests
        .owner
        .begin_turn(&"s".into(), &"r".into())
        .expect("#1307 admission slot");
    let steer = steer_resource(&requests);
    let _turn = start_steer_run(&steer, "s", "t", "r");
    // The empty-attachment lane keeps the pre-#1444 raw message shape.
    // Distinct item ids: the mailbox's durable claim gate discards a repeat
    // id within one turn (A7.2-STEER-INPUT-001 差异 6).
    for (reading, fields) in [
        (
            "number message",
            vec![
                ("sessionKey", JsValue::from("s")),
                ("runId", JsValue::from("r")),
                ("itemId", JsValue::from("i0")),
                ("message", JsValue::Number(5.0)),
            ],
        ),
        (
            "null message",
            vec![
                ("sessionKey", JsValue::from("s")),
                ("runId", JsValue::from("r")),
                ("itemId", JsValue::from("i1")),
                ("message", JsValue::Null),
            ],
        ),
        (
            "absent message",
            vec![
                ("sessionKey", JsValue::from("s")),
                ("runId", JsValue::from("r")),
                ("itemId", JsValue::from("i2")),
            ],
        ),
    ] {
        let mut fields = fields;
        let expected_text = match reading {
            "number message" => JsValue::Number(5.0),
            "null message" => JsValue::Null,
            _ => JsValue::Undefined,
        };
        let result = requests
            .request(
                "steer_turn".into(),
                crate::metadata::object(fields.drain(..)),
            )
            .await
            .unwrap_or_else(|_| panic!("#1444 {reading} must stay Ok"));
        negative_assert_eq!(
            result.get("accepted"),
            Some(&JsValue::Bool(true)),
            "#1444 {reading} keeps the plain turn"
        );
        let drained = steer
            .mailbox_for_test(&"s".into())
            .expect("#1307 session mailbox")
            .drain(&"t".into());
        negative_assert_eq!(
            drained.len(),
            1,
            "#1444 {reading} enqueues exactly one steer"
        );
        negative_assert_eq!(
            drained[0].message.get("content"),
            Some(&JsValue::Array(vec![crate::metadata::object([
                ("type", "text".into()),
                ("text", expected_text),
            ])])),
            "#1444 {reading} embeds the raw wire value in the text block"
        );
    }
    // Non-empty attachments + non-string message: the submit-lane guard
    // rejects instead of composing the TS shape (residual, probe a12).
    let error = requests
        .request(
            "steer_turn".into(),
            crate::metadata::object([
                ("sessionKey", "s".into()),
                ("runId", "r".into()),
                ("itemId", "i".into()),
                ("message", JsValue::Number(5.0)),
                (
                    "attachments",
                    JsValue::Array(vec![crate::metadata::object([
                        ("type", "text".into()),
                        ("content", "attachment body".into()),
                    ])]),
                ),
            ]),
        )
        .await
        .expect_err("#1444 the submit-lane message guard fires for attachments");
    negative_assert_eq!(
        error.code,
        "gateway_submit_failed",
        "#1444 the guard keeps the submit error code"
    );
    negative_assert_eq!(
        error.message,
        pilotdeck_protocol::JsString::from("message must be a string."),
        "#1444 the guard keeps the submit message text"
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
async fn steer_turn_registers_leased_attachment_paths() {
    // #1444: through the submit-lane prepare a channel-registered path
    // attachment inlines its body, appends the session path note, and
    // registers allowedReadFiles exactly like TS steerTurn (probe a18).
    use crate::runtime::turn::gateway::GatewayRequests;

    let (_scratch, requests) = std::panic::catch_unwind(super::super::management_tests::setup)
        .expect("#1307 registry fixture setup");
    let lease_dir = requests
        .options
        .project
        .join(".tmp")
        .join("chat-upload-leases")
        .join("u1");
    std::fs::create_dir_all(&lease_dir).expect("#1444 lease directory");
    let path = lease_dir.join("note.txt");
    // No trailing newline: the inline template adds its own separator lines
    // (probe a18's file was written the same way).
    std::fs::write(&path, "note body").expect("#1444 lease file");
    let path = path.to_str().expect("#1444 lease UTF-8").to_owned();
    let _reservation = requests
        .owner
        .begin_turn(&"s".into(), &"r".into())
        .expect("#1307 admission slot");
    let steer = steer_resource(&requests);
    let _turn = start_steer_run(&steer, "s", "t", "r");
    let result = requests
        .request(
            "steer_turn".into(),
            crate::metadata::object([
                ("sessionKey", "s".into()),
                ("runId", "r".into()),
                ("itemId", "reg".into()),
                ("message", "hello".into()),
                (
                    "attachments",
                    JsValue::Array(vec![crate::metadata::object([
                        ("path", path.as_str().into()),
                        ("mimeType", "text/plain".into()),
                        (
                            "metadata",
                            crate::metadata::object([("channelKey", "chan-1".into())]),
                        ),
                    ])]),
                ),
            ]),
        )
        .await
        .expect("#1444 a registered attachment composes the steer");
    negative_assert_eq!(
        result.get("accepted"),
        Some(&JsValue::Bool(true)),
        "#1444 a registered attachment does not block the steer"
    );
    let drained = steer
        .mailbox_for_test(&"s".into())
        .expect("#1307 session mailbox")
        .drain(&"t".into());
    negative_assert_eq!(
        drained.len(),
        1,
        "#1444 exactly the composed steer message is queued"
    );
    let delivered = &drained[0];
    let blocks = match delivered.message.get("content") {
        Some(JsValue::Array(blocks)) => blocks.clone(),
        other => panic!("#1444 composed content must be an array, got {other:?}"),
    };
    negative_assert_eq!(
        blocks.len(),
        3,
        "#1444 message + inlined body + path note (probe a18)"
    );
    negative_assert_eq!(
        blocks[0].get("text"),
        Some(&JsValue::from("hello")),
        "#1444 the message text block leads"
    );
    negative_assert_eq!(
        blocks[1].get("text"),
        Some(&JsValue::from(
            format!("<attachment path=\"{path}\">\nnote body\n</attachment>").as_str()
        )),
        "#1444 the lease file body is inlined with its path reference"
    );
    let note = match blocks[2]
        .get("text")
        .and_then(|value| value.as_string().and_then(|text| text.to_utf8().ok()))
    {
        Some(note) => note,
        None => panic!("#1444 the path note must be a text block"),
    };
    negative_assert_eq!(
        note,
        format!(
            "\n\n[Registered attachment files in this session:]\n- note.txt: {path}\n\
             These are path references for reuse. If an image/PDF is already visible in this \
             turn, do not call read_file just to view it."
        ),
        "#1444 the registered path note matches the TS composer shape"
    );
    negative_assert_eq!(
        delivered.allowed_read_files,
        Some(vec![pilotdeck_protocol::JsString::from(path.as_str())]),
        "#1444 the lease path is registered in allowedReadFiles"
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
async fn steer_turn_attachments_reuse_the_profile_capability_gate() {
    // #1444 residual 2: the non-empty-attachment lane reuses the submit-side
    // prepare, whose profile validation gates the request — TS steerTurn
    // composes through the plain composer with no profile concept (ledger
    // A7.2-STEER-ATTACH-001-R2 差异 2). Without attachments the inline path
    // never consults the profile and is unaffected.
    use crate::runtime::turn::gateway::GatewayRequests;

    let (_scratch, requests) = std::panic::catch_unwind(super::super::management_tests::setup)
        .expect("#1307 registry fixture setup");
    // A configured plugin directory makes validate_profile refuse the
    // isolated native profile (assembly.rs).
    std::fs::create_dir_all(requests.options.home.join("plugins"))
        .expect("#1444 plugins directory");
    std::fs::write(requests.options.home.join("plugins/x.js"), "").expect("#1444 plugin file");
    let _reservation = requests
        .owner
        .begin_turn(&"s".into(), &"r".into())
        .expect("#1307 admission slot");
    let steer = steer_resource(&requests);
    let _turn = start_steer_run(&steer, "s", "t", "r");
    // Without attachments the inline path never consults the profile.
    let result = requests
        .request(
            "steer_turn".into(),
            crate::metadata::object([
                ("sessionKey", "s".into()),
                ("runId", "r".into()),
                ("itemId", "i".into()),
                ("message", "hello".into()),
            ]),
        )
        .await
        .expect("#1444 the inline path ignores the profile");
    negative_assert_eq!(
        result.get("accepted"),
        Some(&JsValue::Bool(true)),
        "#1444 a plain steer stays accepted under an invalid profile"
    );
    // With attachments the submit-lane capability gate fires instead of
    // composing.
    let error = requests
        .request(
            "steer_turn".into(),
            crate::metadata::object([
                ("sessionKey", "s".into()),
                ("runId", "r".into()),
                ("itemId", "i".into()),
                ("message", "hello".into()),
                (
                    "attachments",
                    JsValue::Array(vec![crate::metadata::object([
                        ("type", "text".into()),
                        ("content", "attachment body".into()),
                    ])]),
                ),
            ]),
        )
        .await
        .expect_err("#1444 the submit-lane profile gate fires for attachments");
    negative_assert_eq!(
        error.code,
        "CAPABILITY_UNAVAILABLE",
        "#1444 the profile gate keeps the capability error code"
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
async fn steer_turn_profile_gate_does_not_block_the_serial_host_executor() {
    // #1444 fix-r1 (review MM concurrency #2): the request future is
    // polled by the current-thread turn-gateway host
    // (`ConnectionTurnGateway::start_with_clock_and_cleanup` builds
    // `new_current_thread()`), so the profile gate's FS sweep (the MCP
    // config load, the plugin/skill `read_dir`s and the project config
    // load) now runs inside `spawn_blocking` — the same treatment the
    // composition gets. A steer with non-empty attachments whose config
    // load parks on a FIFO (in place of the home `pilotdeck.yaml`) must
    // leave the executor free: the 10ms probe below completes while the
    // load is still parked, before the release write. The request itself
    // must still complete with the composed turn once the FIFO is fed the
    // fixture's own config bytes.
    use crate::runtime::turn::gateway::GatewayRequests;
    use std::io::Write as _;

    let (_scratch, requests) = std::panic::catch_unwind(super::super::management_tests::setup)
        .expect("#1307 registry fixture setup");
    let _reservation = requests
        .owner
        .begin_turn(&"s".into(), &"r".into())
        .expect("#1307 admission slot");
    let steer = steer_resource(&requests);
    let _turn = start_steer_run(&steer, "s", "t", "r");
    let config_path = requests.options.home.join("pilotdeck.yaml");
    let config = std::fs::read(&config_path).expect("#1444 fixture config bytes");
    std::fs::remove_file(&config_path).expect("#1444 drop the fixture config for the FIFO");
    negative_assert!(
        std::process::Command::new("mkfifo")
            .arg(&config_path)
            .status()
            .expect("#1444 mkfifo launch")
            .success(),
        "#1444 config FIFO fixture"
    );
    // The releaser's FIFO open only succeeds once the reader opened, so a
    // successful rendezvous proves the config load is parked in the read.
    // It then waits for the probe's completion signal; if the executor
    // were blocked inline the probe could never finish first, and the
    // bounded wait releases the read either way so the test cannot hang.
    let (parked, parked_rx) = tokio::sync::oneshot::channel::<()>();
    let (probe_done, probe_done_rx) = std::sync::mpsc::channel::<()>();
    let releaser = std::thread::Builder::new()
        .name("pdrs-1444-fifo-releaser".into())
        .spawn(move || {
            let mut writer = std::fs::OpenOptions::new()
                .write(true)
                .open(&config_path)
                .expect("#1444 FIFO writer rendezvous");
            let _ = parked.send(());
            let probe_first = probe_done_rx
                .recv_timeout(std::time::Duration::from_millis(500))
                .is_ok();
            writer
                .write_all(&config)
                .expect("#1444 release the config read");
            drop(writer);
            probe_first
        })
        .expect("#1444 releaser thread");
    let request = requests.request(
        "steer_turn".into(),
        crate::metadata::object([
            ("sessionKey", "s".into()),
            ("runId", "r".into()),
            ("itemId", "i".into()),
            ("message", "hello".into()),
            (
                "attachments",
                JsValue::Array(vec![crate::metadata::object([
                    ("type", "text".into()),
                    ("content", "attachment body".into()),
                ])]),
            ),
        ]),
    );
    // The probe: once the config read is parked, a 10ms sleep must
    // complete on this executor while the load is still blocked.
    let (result, ()) = tokio::join!(request, async {
        parked_rx
            .await
            .expect("#1444 the config read reached the FIFO");
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        let _ = probe_done.send(());
    });
    let probe_first = releaser.join().expect("#1444 releaser join");
    negative_assert!(
        probe_first,
        "#1444 the profile gate's config load must not block the serial host executor"
    );
    let result = result.expect("#1444 the parked steer still composes");
    negative_assert_eq!(
        result.get("accepted"),
        Some(&JsValue::Bool(true)),
        "#1444 the parked steer is accepted once released"
    );
    let drained = steer
        .mailbox_for_test(&"s".into())
        .expect("#1307 session mailbox")
        .drain(&"t".into());
    negative_assert_eq!(
        drained.len(),
        1,
        "#1444 the parked steer enqueues exactly one message"
    );
    negative_assert_eq!(
        drained[0].message.get("content"),
        Some(&JsValue::Array(vec![
            crate::metadata::object([("type", "text".into()), ("text", "hello".into()),]),
            crate::metadata::object([("type", "text".into()), ("text", "attachment body".into()),]),
        ])),
        "#1444 the parked steer composes message and attachment blocks"
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
async fn steer_turn_skips_non_object_attachment_elements() {
    // #1444 fix-r1 (review DS boundary #1): a real array whose elements are
    // not attachment-shaped objects is filtered element-wise by the
    // composer's passes — TS reads `attachment.path` / `attachment.type` on
    // the raw element, so `[1]`/`["x"]`/`[{}]` skip every pass and compose
    // the plain text turn (probe
    // `_tmp/20261026-issue-1444/fix-r1/ts-probe/output.txt` e2-e4), while a
    // `null` element throws the property-read TypeError there (e1). Rust's
    // field access returns None for those elements and skips them silently,
    // so all four cells accept and deliver the plain turn — the `null` cell
    // is residual 3 (ledger A7.2-STEER-ATTACH-001-R2), the other three are
    // aligned shapes pinned here.
    use crate::runtime::turn::gateway::GatewayRequests;

    let (_scratch, requests) = std::panic::catch_unwind(super::super::management_tests::setup)
        .expect("#1307 registry fixture setup");
    let _reservation = requests
        .owner
        .begin_turn(&"s".into(), &"r".into())
        .expect("#1307 admission slot");
    let steer = steer_resource(&requests);
    let _turn = start_steer_run(&steer, "s", "t", "r");
    let plain_content = JsValue::Array(vec![crate::metadata::object([
        ("type", "text".into()),
        ("text", "hello".into()),
    ])]);
    // Distinct item ids: the mailbox's durable claim gate discards a repeat
    // id within one turn (A7.2-STEER-INPUT-001 差异 6).
    for (index, (reading, element)) in [
        ("null element", JsValue::Null),
        ("number element", JsValue::Number(1.0)),
        ("string element", JsValue::from("x")),
        ("object element", crate::metadata::object([])),
    ]
    .into_iter()
    .enumerate()
    {
        let item_id = format!("i{index}");
        let result = requests
            .request(
                "steer_turn".into(),
                crate::metadata::object([
                    ("sessionKey", "s".into()),
                    ("runId", "r".into()),
                    ("itemId", JsValue::from(item_id.as_str())),
                    ("message", "hello".into()),
                    ("attachments", JsValue::Array(vec![element])),
                ]),
            )
            .await
            .unwrap_or_else(|_| panic!("#1444 {reading} must stay Ok"));
        negative_assert_eq!(
            result.get("accepted"),
            Some(&JsValue::Bool(true)),
            "#1444 {reading} keeps the plain turn"
        );
    }
    let drained = steer
        .mailbox_for_test(&"s".into())
        .expect("#1307 session mailbox")
        .drain(&"t".into());
    negative_assert_eq!(
        drained.len(),
        4,
        "#1444 each non-object element spelling enqueues exactly one steer"
    );
    for delivered in &drained {
        negative_assert_eq!(
            delivered.message.get("content"),
            Some(&plain_content),
            "#1444 non-object elements are skipped, the plain text turn composes"
        );
        negative_assert_eq!(
            delivered.allowed_read_files,
            Some(Vec::new()),
            "#1444 non-object elements register no read files"
        );
    }
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
    use crate::runtime::async_hooks::{
        AsyncHookRegistry, AsyncHookTurn, AsyncHookTurnHandle, TokioHookClock,
    };
    use crate::runtime::steer::{EnqueueResult, SteerMailbox, SteerMessage};
    use crate::runtime::turn::gateway::GatewayRequests;

    // A bare-mailbox delivery handle; this test only pins the blank-id error
    // shape, so the registry lookup never runs.
    struct BareTurn {
        run_id: pilotdeck_protocol::JsString,
        identity: std::sync::Arc<()>,
        mailbox: SteerMailbox,
    }
    impl AsyncHookTurnHandle for BareTurn {
        fn run_id(&self) -> &pilotdeck_protocol::JsString {
            &self.run_id
        }
        fn identity(&self) -> &std::sync::Arc<()> {
            &self.identity
        }
        fn is_live(&self) -> bool {
            true
        }
        fn enqueue(&self, message: SteerMessage) -> crate::host::HostFuture<EnqueueResult> {
            self.mailbox.enqueue(self.run_id.clone(), message)
        }
    }

    let (_scratch, requests) = std::panic::catch_unwind(super::super::management_tests::setup)
        .expect("#1307 registry fixture setup");
    let mut with_registry = requests.clone();
    let mailbox = SteerMailbox::new(None);
    mailbox.start("t".into());
    let identity = std::sync::Arc::new(());
    with_registry.async_hook_registry = Some(std::sync::Arc::new(AsyncHookRegistry::new(
        std::sync::Arc::new(TokioHookClock),
        move |_: &pilotdeck_protocol::JsString| {
            Some(AsyncHookTurn::new(BareTurn {
                run_id: "t".into(),
                identity: identity.clone(),
                mailbox: mailbox.clone(),
            }))
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

    // grant_session_permission has NO input validation in TS
    // (GatewayInteractionCoordinator.ts:128-134): a missing sessionKey still
    // grants, and a non-string entry throws `entry.trim is not a function`
    // at the boundary (#1465 TS probes).
    let granted = requests
        .permission_request(
            "grant_session_permission".into(),
            crate::metadata::object([("entry", "Bash".into())]),
        )
        .await
        .expect("#1465 grant accepts a missing sessionKey");
    negative_assert_eq!(
        granted.get("granted"),
        Some(&JsValue::Bool(true)),
        "#1465 grant without sessionKey still grants"
    );
    negative_assert_eq!(
        granted.get("entry"),
        Some(&JsValue::from("Bash")),
        "#1465 grant echoes the entry"
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
        .expect_err("#1465 grant rejects non-string entry");
    negative_assert_eq!(
        error.code,
        "gateway_request_failed",
        "#1465 grant non-string entry is a transport failure"
    );
    negative_assert_eq!(
        message(&error),
        "entry.trim is not a function",
        "#1465 grant non-string entry message matches TS"
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

/// #1465 / #1593 gate: a grant whose `sessionKey` is not a string, or is
/// exactly `""`, must not land in the `""` rule set. TS keys
/// `permissionGrants` by the raw value, so `5` and `undefined` are entries of
/// their own and never alias a real session. This registry is keyed by the
/// lossy string form instead and a turn sent without a `sessionKey` acquires
/// `""` (`runtime/turn/submit.rs:330` → `native_process/assembly.rs:247`), so
/// writing there would let malformed input whitelist tools for unrelated
/// sessionless turns (measured before the fix: the `""` allow list grew by one
/// entry per malformed grant). `""` is the only *string* key with that problem;
/// a whitespace-only key is a bucket of its own on both the grant and the turn
/// side, so it must still be written.
#[tokio::test]
async fn malformed_grant_does_not_reach_the_sessionless_bucket() {
    let (_scratch, requests) = std::panic::catch_unwind(super::super::management_tests::setup)
        .expect("#1465 malformed grant fixture setup");
    let empty = pilotdeck_protocol::JsString::from("");
    negative_assert_eq!(
        requests
            .interactions
            .session_allow_rules(&empty)
            .snapshot()
            .len(),
        0,
        "#1465 the sessionless rule set starts empty"
    );

    for (label, session_key) in [
        ("number", JsValue::Number(5.0)),
        ("undefined", JsValue::Undefined),
        ("bool", JsValue::Bool(true)),
        ("empty string", JsValue::String(empty.clone())),
    ] {
        let granted = requests
            .permission_request(
                "grant_session_permission".into(),
                crate::metadata::object([("sessionKey", session_key), ("entry", "Bash".into())]),
            )
            .await
            .unwrap_or_else(|_| panic!("#1465 {label} grant keeps the TS response"));
        negative_assert_eq!(
            granted.get("granted"),
            Some(&JsValue::Bool(true)),
            "#1465 malformed grant still answers granted:true like TS"
        );
    }
    negative_assert_eq!(
        requests
            .interactions
            .session_allow_rules(&empty)
            .snapshot()
            .len(),
        0,
        "#1465 malformed grants must not reach the sessionless rule set"
    );

    // A whitespace-only key is NOT the sessionless alias: the turn side keeps
    // it verbatim (`runtime/turn/submit.rs:330`), so TS and Rust both store and
    // read it under its own key. Dropping it would be an over-narrowing.
    let spaces = pilotdeck_protocol::JsString::from("   ");
    let spaced = requests
        .permission_request(
            "grant_session_permission".into(),
            crate::metadata::object([
                ("sessionKey", JsValue::String(spaces.clone())),
                ("entry", "Bash".into()),
            ]),
        )
        .await
        .expect("#1465 whitespace sessionKey grant answers");
    negative_assert_eq!(
        spaced.get("granted"),
        Some(&JsValue::Bool(true)),
        "#1465 whitespace sessionKey still answers granted:true"
    );
    negative_assert_eq!(
        requests
            .interactions
            .session_allow_rules(&spaces)
            .snapshot()
            .len(),
        1,
        "#1465 whitespace sessionKey keeps its own bucket"
    );
    negative_assert_eq!(
        requests
            .interactions
            .session_allow_rules(&empty)
            .snapshot()
            .len(),
        0,
        "#1465 whitespace grant must not alias the sessionless bucket"
    );

    // Blank entry is the `granted:false` arm (TS probe output.txt req-26).
    let blank = requests
        .permission_request(
            "grant_session_permission".into(),
            crate::metadata::object([("sessionKey", "s".into()), ("entry", "".into())]),
        )
        .await
        .expect("#1465 blank entry answers");
    negative_assert_eq!(
        blank.get("granted"),
        Some(&JsValue::Bool(false)),
        "#1465 blank entry is not a grantable rule"
    );

    // A canonical string key still stores the rule in its own bucket.
    let named = requests
        .permission_request(
            "grant_session_permission".into(),
            crate::metadata::object([("sessionKey", "grant-ok".into()), ("entry", "Read".into())]),
        )
        .await
        .expect("#1465 canonical grant answers");
    negative_assert_eq!(
        named.get("granted"),
        Some(&JsValue::Bool(true)),
        "#1465 canonical grant still grants"
    );
    negative_assert_eq!(
        requests
            .interactions
            .session_allow_rules(&pilotdeck_protocol::JsString::from("grant-ok"))
            .snapshot()
            .len(),
        1,
        "#1465 canonical grant lands in its own session bucket"
    );

    requests
        .project
        .dispose()
        .await
        .expect("#1465 malformed grant fixture shutdown");
}

/// #1465: every non-canonical input shape below is a verbatim TS probe frame
/// (`_tmp/20261106-issue-1465/ts-probe/output.txt` and `output-2.txt`).
/// `input.<key>?.trim()` has three outcomes: a nullish `params` object throws
/// while reading the property, a nullish/blank string is the domain error, and
/// any other non-string makes `.trim` throw at the boundary.
#[tokio::test]
async fn noncanonical_input_forms_match_ts_probes() {
    let (_scratch, requests) = std::panic::catch_unwind(super::super::management_tests::setup)
        .expect("#1465 non-canonical fixture setup");
    *requests
        .user_dialog_bus
        .lock()
        .unwrap_or_else(|e| e.into_inner()) = crate::user_dialog::GatewayUserDialogBus::connected(
        crate::user_dialog::DialogRuntime::default(),
    );

    async fn expect_failure(
        requests: &super::super::requests::Requests,
        method: &str,
        params: JsValue,
        code: &str,
        text: &str,
    ) {
        let error = match method {
            "permission_list" | "grant_session_permission" => {
                requests.permission_request(method.into(), params).await
            }
            _ => requests.user_dialog_request(method.into(), params).await,
        }
        .expect_err("#1465 non-canonical input must fail");
        negative_assert_eq!(
            error.code,
            code,
            "#1465 {method} error code matches the TS probe"
        );
        negative_assert_eq!(
            message(&error),
            text,
            "#1465 {method} error message matches the TS probe"
        );
    }

    // list — non-string sessionKey throws; nullish params throws while
    // reading the property; a non-object params yields `undefined`.
    expect_failure(
        &requests,
        "user_dialog_list",
        crate::metadata::object([("sessionKey", JsValue::Number(5.0))]),
        "gateway_request_failed",
        "input.sessionKey?.trim is not a function",
    )
    .await;
    expect_failure(
        &requests,
        "user_dialog_list",
        JsValue::Undefined,
        "gateway_request_failed",
        "Cannot read properties of undefined (reading 'sessionKey')",
    )
    .await;
    expect_failure(
        &requests,
        "user_dialog_list",
        JsValue::Null,
        "gateway_request_failed",
        "Cannot read properties of null (reading 'sessionKey')",
    )
    .await;
    expect_failure(
        &requests,
        "user_dialog_list",
        JsValue::Number(5.0),
        "INVALID_USER_DIALOG_LIST",
        "sessionKey is required.",
    )
    .await;

    // claim — the guard evaluates sessionKey before requestId.
    expect_failure(
        &requests,
        "user_dialog_claim",
        crate::metadata::object([
            ("sessionKey", JsValue::Number(5.0)),
            ("requestId", JsValue::from("r")),
        ]),
        "gateway_request_failed",
        "input.sessionKey?.trim is not a function",
    )
    .await;
    expect_failure(
        &requests,
        "user_dialog_claim",
        crate::metadata::object([
            ("sessionKey", JsValue::from("s")),
            ("requestId", JsValue::Number(5.0)),
        ]),
        "gateway_request_failed",
        "input.requestId?.trim is not a function",
    )
    .await;

    // respond — same guard order; the bus is not gated for respond.
    expect_failure(
        &requests,
        "user_dialog_respond",
        crate::metadata::object([
            ("sessionKey", JsValue::Number(5.0)),
            ("requestId", JsValue::from("r")),
            (
                "result",
                crate::metadata::object([("behavior", JsValue::from("cancelled"))]),
            ),
        ]),
        "gateway_request_failed",
        "input.sessionKey?.trim is not a function",
    )
    .await;
    expect_failure(
        &requests,
        "user_dialog_respond",
        crate::metadata::object([
            ("sessionKey", JsValue::from("s")),
            ("requestId", JsValue::Number(5.0)),
            (
                "result",
                crate::metadata::object([("behavior", JsValue::from("cancelled"))]),
            ),
        ]),
        "gateway_request_failed",
        "input.requestId?.trim is not a function",
    )
    .await;

    // release — same guard order as claim/respond (TS probe frames
    // `[release sessionKey:5]` and `[release requestId:5]`).
    expect_failure(
        &requests,
        "user_dialog_release",
        crate::metadata::object([
            ("sessionKey", JsValue::Number(5.0)),
            ("requestId", JsValue::from("r")),
            ("leaseId", JsValue::from("l")),
        ]),
        "gateway_request_failed",
        "input.sessionKey?.trim is not a function",
    )
    .await;
    expect_failure(
        &requests,
        "user_dialog_release",
        crate::metadata::object([
            ("sessionKey", JsValue::from("s")),
            ("requestId", JsValue::Number(5.0)),
            ("leaseId", JsValue::from("l")),
        ]),
        "gateway_request_failed",
        "input.requestId?.trim is not a function",
    )
    .await;

    // permission_list — same guard as list.
    expect_failure(
        &requests,
        "permission_list",
        crate::metadata::object([("sessionKey", JsValue::Number(5.0))]),
        "gateway_request_failed",
        "input.sessionKey?.trim is not a function",
    )
    .await;
    expect_failure(
        &requests,
        "permission_list",
        JsValue::Undefined,
        "gateway_request_failed",
        "Cannot read properties of undefined (reading 'sessionKey')",
    )
    .await;

    // grant — no sessionKey validation; entry is read (and trimmed) first.
    for session_key in [
        JsValue::Number(5.0),
        crate::metadata::object([]),
        JsValue::from(""),
    ] {
        let granted = requests
            .permission_request(
                "grant_session_permission".into(),
                crate::metadata::object([
                    ("sessionKey", session_key),
                    ("entry", JsValue::from("Bash")),
                ]),
            )
            .await
            .expect("#1465 grant accepts a non-canonical sessionKey");
        negative_assert_eq!(
            granted.get("granted"),
            Some(&JsValue::Bool(true)),
            "#1465 grant ignores the sessionKey shape"
        );
        negative_assert_eq!(
            granted.get("entry"),
            Some(&JsValue::from("Bash")),
            "#1465 grant echoes the entry"
        );
    }
    expect_failure(
        &requests,
        "grant_session_permission",
        crate::metadata::object([("sessionKey", JsValue::from("s"))]),
        "gateway_request_failed",
        "Cannot read properties of undefined (reading 'trim')",
    )
    .await;
    expect_failure(
        &requests,
        "grant_session_permission",
        crate::metadata::object([("sessionKey", JsValue::from("s")), ("entry", JsValue::Null)]),
        "gateway_request_failed",
        "Cannot read properties of null (reading 'trim')",
    )
    .await;
    expect_failure(
        &requests,
        "grant_session_permission",
        JsValue::Undefined,
        "gateway_request_failed",
        "Cannot read properties of undefined (reading 'entry')",
    )
    .await;

    requests
        .owner
        .shutdown()
        .await
        .expect("#1465 non-canonical fixture owner shutdown");
    requests
        .project
        .dispose()
        .await
        .expect("#1465 non-canonical fixture project shutdown");
}

#[tokio::test]
async fn user_dialog_capability_gate_precedes_parameter_validation() {
    // This case needs the UNCONNECTED bus: it is the only state that answers
    // CAPABILITY_UNAVAILABLE, and the shared fixture no longer builds it
    // (#1600 — production is always connected). The legacy observer-coupled
    // `new(runtime, None)` is therefore constructed explicitly here, so the
    // capability error must win over the missing-sessionKey domain error even
    // though the shared `require_user_dialog_bus` gate runs before any
    // parameter validation.
    let (_scratch, requests) = std::panic::catch_unwind(super::super::management_tests::setup)
        .expect("#1309 gate-order fixture setup");
    *requests
        .user_dialog_bus
        .lock()
        .unwrap_or_else(|e| e.into_inner()) = crate::user_dialog::GatewayUserDialogBus::new(
        crate::user_dialog::DialogRuntime::default(),
        None,
    );
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
async fn connected_bus_serves_zero_pending_user_dialog_semantics() {
    // #1445: production builds a connected bus with no observer. Availability
    // is method presence (TS GatewayWsConnection.ts:481-490), so every method
    // must answer its real zero-pending semantics instead of
    // CAPABILITY_UNAVAILABLE.
    //
    // #1600: this case deliberately uses the shared fixture's bus as-is. The
    // fixture now assembles the same connected state production does, so
    // reverting it to the observer-coupled `new(runtime, None)` turns these
    // calls red — the fixture cannot silently drift back to a state that no
    // longer exists in production.
    let (_scratch, requests) = std::panic::catch_unwind(super::super::management_tests::setup)
        .expect("#1445 connected-bus fixture setup");
    let session = JsValue::from("connected-session");
    let request = JsValue::from("connected-request");

    let listed = requests
        .user_dialog_request(
            "user_dialog_list".into(),
            crate::metadata::object([("sessionKey", session.clone())]),
        )
        .await
        .expect("#1445 list serves a connected bus");
    assert_eq!(
        listed.get("dialogs").and_then(|value| match value {
            JsValue::Array(items) => Some(items.len()),
            _ => None,
        }),
        Some(0),
        "#1445 list on a zero-pending bus returns an empty dialogs array"
    );

    let claimed = requests
        .user_dialog_request(
            "user_dialog_claim".into(),
            crate::metadata::object([
                ("sessionKey", session.clone()),
                ("requestId", request.clone()),
            ]),
        )
        .await
        .expect("#1445 claim serves a connected bus");
    negative_assert_eq!(
        claimed.get("claimed"),
        Some(&JsValue::Bool(false)),
        "#1445 claim on a zero-pending bus reports not claimed"
    );
    negative_assert_eq!(
        claimed.get("reason"),
        Some(&JsValue::from("not_pending")),
        "#1445 claim on a zero-pending bus reports not_pending"
    );

    let released = requests
        .user_dialog_request(
            "user_dialog_release".into(),
            crate::metadata::object([
                ("sessionKey", session.clone()),
                ("requestId", request.clone()),
                ("leaseId", JsValue::from("lease-1")),
            ]),
        )
        .await
        .expect("#1445 release serves a connected bus");
    negative_assert_eq!(
        released.get("released"),
        Some(&JsValue::Bool(false)),
        "#1445 release on a zero-pending bus reports not released"
    );

    let responded = requests
        .user_dialog_request(
            "user_dialog_respond".into(),
            crate::metadata::object([
                ("sessionKey", session),
                ("requestId", request),
                (
                    "result",
                    crate::metadata::object([("behavior", JsValue::from("cancelled"))]),
                ),
            ]),
        )
        .await
        .expect("#1445 respond serves a connected bus");
    negative_assert_eq!(
        responded.get("delivered"),
        Some(&JsValue::Bool(false)),
        "#1445 respond on a zero-pending bus reports not delivered"
    );

    requests
        .owner
        .shutdown()
        .await
        .expect("#1445 connected-bus fixture owner shutdown");
    requests
        .project
        .dispose()
        .await
        .expect("#1445 connected-bus fixture project shutdown");
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
                accepts: Arc::new(|_value| true),
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
                accepts: Arc::new(move |value| std::matches!(value, JsValue::Null) == accepts_null),
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

#[tokio::test]
async fn user_dialog_respond_validates_result_before_lease_id() {
    use crate::metadata::object;

    let (_scratch, requests) = std::panic::catch_unwind(super::super::management_tests::setup)
        .expect("#1453 guard-order fixture setup");
    let behavior_error = "result.behavior must be answered or cancelled.";
    let value_error = "answered user dialog results require a value.";
    let reason_error = "cancelled user dialog reason must be a string.";
    let lease_error = "leaseId must be a non-empty string when provided.";
    let cases = [
        ("missing-result", None, behavior_error),
        ("null-result", Some(JsValue::Null), behavior_error),
        ("missing-behavior", Some(object([])), behavior_error),
        (
            "invalid-behavior",
            Some(object([("behavior", "invalid".into())])),
            behavior_error,
        ),
        (
            "answered-missing-value",
            Some(object([("behavior", "answered".into())])),
            value_error,
        ),
        (
            "answered-undefined-value",
            Some(object([
                ("behavior", "answered".into()),
                ("value", JsValue::Undefined),
            ])),
            value_error,
        ),
        (
            "cancelled-null-reason",
            Some(object([
                ("behavior", "cancelled".into()),
                ("reason", JsValue::Null),
            ])),
            reason_error,
        ),
        (
            "answered-null-value",
            Some(object([
                ("behavior", "answered".into()),
                ("value", JsValue::Null),
            ])),
            lease_error,
        ),
        (
            "cancelled-missing-reason",
            Some(object([("behavior", "cancelled".into())])),
            lease_error,
        ),
        (
            "cancelled-string-reason",
            Some(object([
                ("behavior", "cancelled".into()),
                ("reason", "".into()),
            ])),
            lease_error,
        ),
    ];
    for (name, result, expected_message) in cases {
        let mut fields = vec![
            ("sessionKey", "guard-order-session".into()),
            ("requestId", "guard-order-request".into()),
            ("leaseId", JsValue::Null),
        ];
        if let Some(result) = result {
            fields.push(("result", result));
        }
        let error = requests
            .user_dialog_request("user_dialog_respond".into(), object(fields))
            .await
            .expect_err("#1453 guard-order invalid input must fail");
        assert_eq!(
            error.code, "INVALID_USER_DIALOG_RESPONSE",
            "#1453 guard-order error code: {name}"
        );
        assert_eq!(
            error
                .message
                .to_utf8()
                .expect("#1453 guard-order error message UTF-8"),
            expected_message,
            "#1453 guard-order error message: {name}"
        );
    }
    requests
        .owner
        .shutdown()
        .await
        .expect("#1453 guard-order owner shutdown");
    requests
        .project
        .dispose()
        .await
        .expect("#1453 guard-order project shutdown");
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

// #1437: method-layer positive/negative controls for the permission and
// user_dialog family handlers (methods/permission.rs, methods/user_dialog.rs).
// Append-only section; the #1309 tests above stay untouched.

/// Wire a connected bus (on_change supplied → is_connected) like the #1309
/// fix-r5/r6 fixtures, so the method layer runs its parameter validation
/// instead of the capability gate.
fn wire_user_dialog_bus_1437(requests: &mut super::super::requests::Requests) {
    requests.user_dialog_bus = std::sync::Arc::new(std::sync::Mutex::new(
        crate::user_dialog::GatewayUserDialogBus::new(
            crate::user_dialog::DialogRuntime::default(),
            Some(Box::new(|_event| Ok(()))),
        ),
    ));
}

fn register_dialog_1437(
    requests: &super::super::requests::Requests,
    session: &JsString,
    request_id: &str,
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
                tool_call_id: "call-1437".into(),
                tool_name: "ask".into(),
                event: crate::metadata::object([("requestId", JsValue::String(request_id.into()))]),
                accepts: Arc::new(|_value| true),
                resolve: Box::new(move |answer| sink.lock().unwrap().push(answer)),
                reject: Box::new(|_error| {}),
                on_settled: None,
            },
        );
}

// #1437: both families are gateway-request methods, not verified native
// metadata methods, so they must stay outside LOCAL_METHODS like the upload
// family (precedent: upload_gateway_methods_stay_outside_the_default_dispatch_
// table, upload/gateway_tests.rs:841).
#[test]
fn permission_user_dialog_methods_stay_outside_local_dispatch() {
    for method in [
        "permission_list",
        "grant_session_permission",
        "user_dialog_list",
        "user_dialog_claim",
        "user_dialog_release",
        "user_dialog_respond",
    ] {
        let request = crate::metadata::object([
            ("type", "request".into()),
            ("id", "1".into()),
            ("method", method.into()),
        ]);
        negative_assert!(
            !crate::dispatch::is_local(&request),
            "#1437 {method} must stay outside LOCAL_METHODS"
        );
    }
}

// #1437: a present-but-blank sessionKey ("  ") is not a missing key, and the
// two permission methods split on it exactly like TS does:
//
// * `permission_list` guards with `!input.sessionKey?.trim()`
//   (InProcessGateway.ts:2781-2783), so a blank key keeps the domain
//   rejection;
// * `grant_session_permission` performs no such guard (`#1465`): the
//   coordinator keys the grant Map by the raw sessionKey, so a whitespace-only
//   key is stored in its own bucket. Only the exact `""` string is suppressed,
//   because the turn side maps a missing/non-string sessionKey onto `""`
//   (`native_process/assembly.rs:247`) and would otherwise whitelist tools for
//   unrelated sessionless turns.
#[tokio::test]
async fn permission_blank_session_key_splits_by_surface() {
    let (_scratch, requests) = std::panic::catch_unwind(super::super::management_tests::setup)
        .expect("#1437 permission blank fixture setup");
    let error = requests
        .permission_request(
            "permission_list".into(),
            crate::metadata::object([("sessionKey", "  ".into())]),
        )
        .await
        .expect_err("#1437 permission_list rejects a blank sessionKey");
    negative_assert_eq!(
        error.code,
        "INVALID_PERMISSION_LIST",
        "#1437 permission_list blank sessionKey keeps the domain code"
    );
    negative_assert_eq!(
        message(&error),
        "sessionKey is required.",
        "#1437 permission_list blank sessionKey message"
    );
    let granted = requests
        .permission_request(
            "grant_session_permission".into(),
            crate::metadata::object([("sessionKey", "\t".into()), ("entry", "Bash".into())]),
        )
        .await
        .expect("#1437 grant keeps a whitespace sessionKey");
    negative_assert_eq!(
        granted.get("granted"),
        Some(&JsValue::Bool(true)),
        "#1437 grant whitespace sessionKey answers granted:true"
    );
    let spaces = pilotdeck_protocol::JsString::from("\t");
    let empty = pilotdeck_protocol::JsString::from("");
    negative_assert_eq!(
        requests
            .interactions
            .session_allow_rules(&spaces)
            .snapshot()
            .len(),
        1,
        "#1437 grant keeps a whitespace sessionKey in its own bucket"
    );
    negative_assert_eq!(
        requests
            .interactions
            .session_allow_rules(&empty)
            .snapshot()
            .len(),
        0,
        "#1437 grant whitespace sessionKey must not alias the sessionless bucket"
    );
    requests
        .owner
        .shutdown()
        .await
        .expect("#1437 permission blank fixture owner shutdown");
    requests
        .project
        .dispose()
        .await
        .expect("#1437 permission blank fixture project shutdown");
}

// #1437: user_dialog_list on a wired bus — the INVALID_USER_DIALOG_LIST domain
// code for a missing/blank sessionKey, and the dialog-event projection for a
// valid one (previously only the capability gate on an unwired bus was
// covered; user_dialog.rs:124).
#[tokio::test]
async fn user_dialog_list_validates_session_key_and_projects_dialogs() {
    let (_scratch, mut requests) = std::panic::catch_unwind(super::super::management_tests::setup)
        .expect("#1437 user_dialog_list fixture setup");
    wire_user_dialog_bus_1437(&mut requests);
    let session: JsString = "1437-list-session".into();
    let resolved: std::sync::Arc<std::sync::Mutex<Vec<crate::user_dialog::DialogAnswer>>> =
        std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    register_dialog_1437(&requests, &session, "1437-dialog", &resolved);

    for (label, params) in [
        ("missing", crate::metadata::object([])),
        (
            "blank",
            crate::metadata::object([("sessionKey", "  ".into())]),
        ),
    ] {
        let error = requests
            .user_dialog_request("user_dialog_list".into(), params)
            .await
            .expect_err(&format!(
                "#1437 user_dialog_list rejects a {label} sessionKey"
            ));
        negative_assert_eq!(
            error.code,
            "INVALID_USER_DIALOG_LIST",
            "#1437 user_dialog_list {label} sessionKey domain code"
        );
        negative_assert_eq!(
            message(&error),
            "sessionKey is required.",
            "#1437 user_dialog_list {label} sessionKey message"
        );
    }

    let result = requests
        .user_dialog_request(
            "user_dialog_list".into(),
            crate::metadata::object([("sessionKey", JsValue::String(session.clone()))]),
        )
        .await
        .expect("#1437 user_dialog_list succeeds on a wired bus");
    let dialogs = result.get("dialogs").and_then(|value| match value {
        JsValue::Array(items) => Some(items),
        _ => None,
    });
    negative_assert!(
        dialogs.is_some(),
        "#1437 user_dialog_list projects a dialogs array"
    );
    let dialogs = dialogs.expect("#1437 user_dialog_list dialogs array");
    negative_assert_eq!(
        dialogs.len(),
        1,
        "#1437 user_dialog_list projects the pending dialog"
    );
    negative_assert_eq!(
        dialogs[0].get("requestId"),
        Some(&JsValue::String("1437-dialog".into())),
        "#1437 user_dialog_list forwards the dialog event"
    );
    negative_assert!(
        dialogs[0].get("lease").is_none(),
        "#1437 an unclaimed dialog carries no lease block"
    );
    requests
        .owner
        .shutdown()
        .await
        .expect("#1437 user_dialog_list fixture owner shutdown");
    requests
        .project
        .dispose()
        .await
        .expect("#1437 user_dialog_list fixture project shutdown");
}

// #1437: ttlMs lease bounds — TS MIN_USER_DIALOG_LEASE_MS = 1_000 and
// MAX_USER_DIALOG_LEASE_MS = 300_000 are inclusive: exactly 1000 and 300000
// claim, one step outside (999 / 300001) and a fractional value are rejected
// with the INVALID_USER_DIALOG_CLAIM domain code.
#[tokio::test]
async fn user_dialog_claim_ttlms_bounds() {
    let (_scratch, mut requests) = std::panic::catch_unwind(super::super::management_tests::setup)
        .expect("#1437 ttlMs bounds fixture setup");
    wire_user_dialog_bus_1437(&mut requests);
    let session: JsString = "1437-ttl-session".into();
    let resolved: std::sync::Arc<std::sync::Mutex<Vec<crate::user_dialog::DialogAnswer>>> =
        std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    for (label, ttl_ms) in [
        ("below", 999.0),
        ("above", 300_001.0),
        ("fractional", 1_000.5),
    ] {
        register_dialog_1437(&requests, &session, "1437-ttl-reject", &resolved);
        let error = requests
            .user_dialog_request(
                "user_dialog_claim".into(),
                crate::metadata::object([
                    ("sessionKey", JsValue::String(session.clone())),
                    ("requestId", "1437-ttl-reject".into()),
                    ("ttlMs", JsValue::Number(ttl_ms)),
                ]),
            )
            .await
            .expect_err(&format!("#1437 {label} ttlMs is rejected"));
        negative_assert_eq!(
            error.code,
            "INVALID_USER_DIALOG_CLAIM",
            "#1437 {label} ttlMs keeps the domain code"
        );
        negative_assert_eq!(
            message(&error),
            "ttlMs must be a safe integer between 1000 and 300000.",
            "#1437 {label} ttlMs message"
        );
    }
    // The non-number arm (user_dialog.rs `_ =>`) is a separate guard from the
    // range/fraction check and must carry the same domain code and message.
    register_dialog_1437(&requests, &session, "1437-ttl-type", &resolved);
    let error = requests
        .user_dialog_request(
            "user_dialog_claim".into(),
            crate::metadata::object([
                ("sessionKey", JsValue::String(session.clone())),
                ("requestId", "1437-ttl-type".into()),
                ("ttlMs", "1000".into()),
            ]),
        )
        .await
        .expect_err("#1437 a non-number ttlMs is rejected");
    negative_assert_eq!(
        error.code,
        "INVALID_USER_DIALOG_CLAIM",
        "#1437 non-number ttlMs keeps the domain code"
    );
    negative_assert_eq!(
        message(&error),
        "ttlMs must be a safe integer between 1000 and 300000.",
        "#1437 non-number ttlMs message"
    );
    for (label, ttl_ms, request_id) in [
        ("minimum", 1_000.0, "1437-ttl-min"),
        ("maximum", 300_000.0, "1437-ttl-max"),
    ] {
        register_dialog_1437(&requests, &session, request_id, &resolved);
        let result = requests
            .user_dialog_request(
                "user_dialog_claim".into(),
                crate::metadata::object([
                    ("sessionKey", JsValue::String(session.clone())),
                    ("requestId", request_id.into()),
                    ("ttlMs", JsValue::Number(ttl_ms)),
                ]),
            )
            .await
            .unwrap_or_else(|error| panic!("#1437 the {label} bound ttlMs claims: {error:?}"));
        negative_assert_eq!(
            result.get("claimed"),
            Some(&JsValue::Bool(true)),
            "#1437 the {label} bound ttlMs claim succeeds"
        );
        negative_assert!(
            result
                .get("leaseId")
                .and_then(JsValue::as_string)
                .is_some_and(|lease| !lease.to_utf8().unwrap_or_default().is_empty()),
            "#1437 the {label} bound ttlMs claim returns a lease"
        );
    }
    requests
        .owner
        .shutdown()
        .await
        .expect("#1437 ttlMs bounds fixture owner shutdown");
    requests
        .project
        .dispose()
        .await
        .expect("#1437 ttlMs bounds fixture project shutdown");
}

// #1437: the sessionKey/requestId required rejects for claim, release and
// respond on a WIRED bus (the #1309 gate-order test only covered the
// capability gate on an unwired one), plus the whitespace-only leaseId reject
// on claim (the #1309 fix-r5 tri-state test covered null, not blanks).
#[tokio::test]
async fn user_dialog_claim_release_domain_codes() {
    let (_scratch, mut requests) = std::panic::catch_unwind(super::super::management_tests::setup)
        .expect("#1437 claim/release fixture setup");
    wire_user_dialog_bus_1437(&mut requests);
    let session: JsString = "1437-domain-session".into();
    for (label, params) in [
        ("missing sessionKey", crate::metadata::object([])),
        (
            "blank sessionKey",
            crate::metadata::object([("sessionKey", "   ".into()), ("requestId", "1437-r".into())]),
        ),
        (
            "missing requestId",
            crate::metadata::object([("sessionKey", JsValue::String(session.clone()))]),
        ),
    ] {
        let error = requests
            .user_dialog_request("user_dialog_claim".into(), params)
            .await
            .expect_err(&format!("#1437 claim rejects a {label}"));
        negative_assert_eq!(
            error.code,
            "INVALID_USER_DIALOG_CLAIM",
            "#1437 claim {label} domain code"
        );
        negative_assert_eq!(
            message(&error),
            "sessionKey and requestId are required.",
            "#1437 claim {label} message"
        );
    }
    let resolved: std::sync::Arc<std::sync::Mutex<Vec<crate::user_dialog::DialogAnswer>>> =
        std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    register_dialog_1437(&requests, &session, "1437-lease-blank", &resolved);
    let error = requests
        .user_dialog_request(
            "user_dialog_claim".into(),
            crate::metadata::object([
                ("sessionKey", JsValue::String(session.clone())),
                ("requestId", "1437-lease-blank".into()),
                ("leaseId", "   ".into()),
            ]),
        )
        .await
        .expect_err("#1437 claim rejects a whitespace-only leaseId");
    negative_assert_eq!(
        error.code,
        "INVALID_USER_DIALOG_CLAIM",
        "#1437 claim blank leaseId domain code"
    );
    negative_assert_eq!(
        message(&error),
        "leaseId must be a non-empty string when provided.",
        "#1437 claim blank leaseId message"
    );
    for (label, params) in [
        ("missing sessionKey", crate::metadata::object([])),
        (
            "blank sessionKey",
            crate::metadata::object([
                ("sessionKey", "   ".into()),
                ("requestId", "1437-rel".into()),
                ("leaseId", "1437-lease".into()),
            ]),
        ),
        (
            "missing requestId",
            crate::metadata::object([("sessionKey", JsValue::String(session.clone()))]),
        ),
        (
            "blank requestId",
            crate::metadata::object([
                ("sessionKey", JsValue::String(session.clone())),
                ("requestId", "   ".into()),
                ("leaseId", "1437-lease".into()),
            ]),
        ),
        // release requires all three fields (user_dialog.rs release guard):
        // unlike claim/respond, a missing or blank leaseId is a domain reject
        // with the same message, not a default/no-op.
        (
            "missing leaseId",
            crate::metadata::object([
                ("sessionKey", JsValue::String(session.clone())),
                ("requestId", "1437-rel".into()),
            ]),
        ),
        (
            "blank leaseId",
            crate::metadata::object([
                ("sessionKey", JsValue::String(session.clone())),
                ("requestId", "1437-rel".into()),
                ("leaseId", "   ".into()),
            ]),
        ),
    ] {
        let error = requests
            .user_dialog_request("user_dialog_release".into(), params)
            .await
            .expect_err(&format!("#1437 release rejects a {label}"));
        negative_assert_eq!(
            error.code,
            "INVALID_USER_DIALOG_RELEASE",
            "#1437 release {label} domain code"
        );
        negative_assert_eq!(
            message(&error),
            "sessionKey, requestId, and leaseId are required.",
            "#1437 release {label} message"
        );
    }
    let error = requests
        .user_dialog_request("user_dialog_respond".into(), crate::metadata::object([]))
        .await
        .expect_err("#1437 respond rejects a missing sessionKey");
    negative_assert_eq!(
        error.code,
        "INVALID_USER_DIALOG_RESPONSE",
        "#1437 respond missing sessionKey domain code"
    );
    negative_assert_eq!(
        message(&error),
        "sessionKey and requestId are required.",
        "#1437 respond missing sessionKey message"
    );
    requests
        .owner
        .shutdown()
        .await
        .expect("#1437 claim/release fixture owner shutdown");
    requests
        .project
        .dispose()
        .await
        .expect("#1437 claim/release fixture project shutdown");
}

// #1437: the respond behavior dispatch — a missing result, a non-string
// behavior and an unknown behavior all share the INVALID_USER_DIALOG_RESPONSE
// rejection; the answered/cancelled behaviors then deliver (the #1309 fix-r5
// test covered value/reason tri-state, not the dispatch itself).
#[tokio::test]
async fn user_dialog_respond_behavior_branches() {
    let (_scratch, mut requests) = std::panic::catch_unwind(super::super::management_tests::setup)
        .expect("#1437 behavior fixture setup");
    wire_user_dialog_bus_1437(&mut requests);
    let session: JsString = "1437-behavior-session".into();
    let resolved: std::sync::Arc<std::sync::Mutex<Vec<crate::user_dialog::DialogAnswer>>> =
        std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    register_dialog_1437(&requests, &session, "1437-dispatch", &resolved);
    for (label, result) in [
        ("missing result", None),
        (
            "non-string behavior",
            Some(crate::metadata::object([(
                "behavior",
                JsValue::Number(1.0),
            )])),
        ),
        (
            "unknown behavior",
            Some(crate::metadata::object([("behavior", "bogus".into())])),
        ),
    ] {
        let mut fields = vec![
            ("sessionKey", JsValue::String(session.clone())),
            ("requestId", "1437-dispatch".into()),
        ];
        if let Some(result) = result {
            fields.push(("result", result));
        }
        let error = requests
            .user_dialog_request(
                "user_dialog_respond".into(),
                crate::metadata::object(fields),
            )
            .await
            .expect_err(&format!("#1437 respond rejects a {label}"));
        negative_assert_eq!(
            error.code,
            "INVALID_USER_DIALOG_RESPONSE",
            "#1437 respond {label} domain code"
        );
        negative_assert_eq!(
            message(&error),
            "result.behavior must be answered or cancelled.",
            "#1437 respond {label} message"
        );
    }
    let result = requests
        .user_dialog_request(
            "user_dialog_respond".into(),
            crate::metadata::object([
                ("sessionKey", JsValue::String(session.clone())),
                ("requestId", "1437-dispatch".into()),
                (
                    "result",
                    crate::metadata::object([
                        ("behavior", "answered".into()),
                        ("value", "yes".into()),
                    ]),
                ),
            ]),
        )
        .await
        .expect("#1437 an answered behavior delivers");
    negative_assert_eq!(
        result.get("delivered"),
        Some(&JsValue::Bool(true)),
        "#1437 an answered behavior reports delivered"
    );
    negative_assert_eq!(
        resolved.lock().unwrap().as_slice(),
        [crate::user_dialog::DialogAnswer::Answered("yes".into())],
        "#1437 an answered behavior reaches the resolver with its value"
    );
    register_dialog_1437(&requests, &session, "1437-cancelled", &resolved);
    let result = requests
        .user_dialog_request(
            "user_dialog_respond".into(),
            crate::metadata::object([
                ("sessionKey", JsValue::String(session.clone())),
                ("requestId", "1437-cancelled".into()),
                (
                    "result",
                    crate::metadata::object([("behavior", "cancelled".into())]),
                ),
            ]),
        )
        .await
        .expect("#1437 a cancelled behavior delivers");
    negative_assert_eq!(
        result.get("delivered"),
        Some(&JsValue::Bool(true)),
        "#1437 a cancelled behavior reports delivered"
    );
    negative_assert_eq!(
        resolved.lock().unwrap().as_slice(),
        [
            crate::user_dialog::DialogAnswer::Answered("yes".into()),
            crate::user_dialog::DialogAnswer::Cancelled(None),
        ],
        "#1437 a cancelled behavior reaches the resolver without a reason"
    );
    requests
        .owner
        .shutdown()
        .await
        .expect("#1437 behavior fixture owner shutdown");
    requests
        .project
        .dispose()
        .await
        .expect("#1437 behavior fixture project shutdown");
}

// #1437: USER_DIALOG_LEASE_REQUIRED — a respond against a dialog claimed by
// another renderer lease is rejected until the matching leaseId is supplied.
// #1453 reorders the user_dialog_respond guards; this test pins the current
// claim-vs-respond lease semantics and needs a sync after that reorder.
#[tokio::test]
async fn user_dialog_respond_lease_required() {
    let (_scratch, mut requests) = std::panic::catch_unwind(super::super::management_tests::setup)
        .expect("#1437 lease-required fixture setup");
    wire_user_dialog_bus_1437(&mut requests);
    let session: JsString = "1437-lease-session".into();
    let resolved: std::sync::Arc<std::sync::Mutex<Vec<crate::user_dialog::DialogAnswer>>> =
        std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    register_dialog_1437(&requests, &session, "1437-lease-dialog", &resolved);
    let claim = requests
        .user_dialog_request(
            "user_dialog_claim".into(),
            crate::metadata::object([
                ("sessionKey", JsValue::String(session.clone())),
                ("requestId", "1437-lease-dialog".into()),
            ]),
        )
        .await
        .expect("#1437 the lease-required claim succeeds");
    let lease = claim
        .get("leaseId")
        .and_then(JsValue::as_string)
        .expect("#1437 the lease-required claim returns a lease");

    let respond = |lease_id: Option<JsValue>| {
        let mut fields = vec![
            ("sessionKey", JsValue::String(session.clone())),
            ("requestId", "1437-lease-dialog".into()),
        ];
        if let Some(value) = lease_id {
            fields.push(("leaseId", value));
        }
        fields.push((
            "result",
            crate::metadata::object([("behavior", "answered".into()), ("value", "ok".into())]),
        ));
        crate::metadata::object(fields)
    };
    for (label, lease_id) in [
        ("without a lease", None),
        ("with a wrong lease", Some("not-the-lease".into())),
    ] {
        let error = requests
            .user_dialog_request("user_dialog_respond".into(), respond(lease_id))
            .await
            .expect_err(&format!("#1437 respond {label} is lease-required"));
        negative_assert_eq!(
            error.code,
            "USER_DIALOG_LEASE_REQUIRED",
            "#1437 respond {label} domain code"
        );
        negative_assert_eq!(
            message(&error),
            "User dialog 1437-lease-dialog is currently claimed by another renderer.",
            "#1437 respond {label} message"
        );
    }
    let result = requests
        .user_dialog_request(
            "user_dialog_respond".into(),
            respond(Some(JsValue::String(lease.clone()))),
        )
        .await
        .expect("#1437 respond with the matching lease delivers");
    negative_assert_eq!(
        result.get("delivered"),
        Some(&JsValue::Bool(true)),
        "#1437 respond with the matching lease reports delivered"
    );
    negative_assert_eq!(
        resolved.lock().unwrap().as_slice(),
        [crate::user_dialog::DialogAnswer::Answered("ok".into())],
        "#1437 respond with the matching lease resolves the dialog"
    );
    requests
        .owner
        .shutdown()
        .await
        .expect("#1437 lease-required fixture owner shutdown");
    requests
        .project
        .dispose()
        .await
        .expect("#1437 lease-required fixture project shutdown");
}

// #1437: user_dialog_respond is the one family method that is NOT capability
// gated (require_user_dialog_bus only guards list/claim/release): a respond
// for a request that is not pending returns Ok({"delivered": false}), never
// Err(CAPABILITY_UNAVAILABLE). The shared fixture has been connected since
// #1600 (like production), so this case rebuilds the unconnected bus
// explicitly — otherwise the state in the test's name would no longer be
// covered. The assertion pins both the method-layer contract and the
// unconnected availability.
#[tokio::test]
async fn user_dialog_respond_unwired_bus_returns_delivered_false() {
    let (_scratch, requests) = std::panic::catch_unwind(super::super::management_tests::setup)
        .expect("#1437 unwired bus fixture setup");
    // Same explicit unconnected construction as
    // `user_dialog_capability_gate_precedes_parameter_validation` (#1600).
    *requests
        .user_dialog_bus
        .lock()
        .unwrap_or_else(|e| e.into_inner()) = crate::user_dialog::GatewayUserDialogBus::new(
        crate::user_dialog::DialogRuntime::default(),
        None,
    );
    for (label, result) in [
        (
            "answered",
            crate::metadata::object([("behavior", "answered".into()), ("value", "yes".into())]),
        ),
        (
            "cancelled",
            crate::metadata::object([("behavior", "cancelled".into())]),
        ),
    ] {
        let value = requests
            .user_dialog_request(
                "user_dialog_respond".into(),
                crate::metadata::object([
                    ("sessionKey", "1437-unwired-session".into()),
                    ("requestId", "1437-unwired-dialog".into()),
                    ("result", result),
                ]),
            )
            .await
            .expect("#1437 respond on an unwired bus stays Ok");
        negative_assert_eq!(
            value.get("delivered"),
            Some(&JsValue::Bool(false)),
            "#1437 respond {label} on an unwired bus reports delivered:false"
        );
    }
    requests
        .owner
        .shutdown()
        .await
        .expect("#1437 unwired bus fixture owner shutdown");
    requests
        .project
        .dispose()
        .await
        .expect("#1437 unwired bus fixture project shutdown");
}

// #1328 U12 `prepare_weixin_login` shapes. TS pins the guard and the wire
// dispatch case: no callback → `{requested: false, requestedAt: <now ISO>,
// reason: "unsupported"}` (InProcessGateway.ts:2912-2921); a configured
// callback's result is forwarded as-is, once per call, and `params` are
// ignored by the dispatch case (GatewayWsConnection.ts:528-536).
fn iso_keys(value: &JsValue) -> Vec<String> {
    value
        .as_object()
        .expect("#1328 result is an object")
        .entries()
        .iter()
        .map(|(key, _)| key.to_utf8().expect("#1328 key UTF-8").to_string())
        .collect()
}

fn parse_iso(text: &JsString) -> Option<f64> {
    use pilotdeck_transport::{server::ExecutionClock, system_clock::SystemExecutionClock};
    SystemExecutionClock
        .parse_date(text)
        .expect("#1328 requestedAt must parse or report why")
}

#[tokio::test]
async fn prepare_weixin_login_without_callback_answers_unsupported() {
    use crate::runtime::turn::gateway::GatewayRequests;

    let (_scratch, requests) = std::panic::catch_unwind(super::super::management_tests::setup)
        .expect("#1328 registry fixture setup");
    // The wire method answers Ok — not metadata::unavailable — with the exact
    // unsupported shape and field order, ignoring whatever params carry.
    let result = requests
        .request(
            "prepare_weixin_login".into(),
            crate::metadata::object([("junk", "noise".into())]),
        )
        .await
        .expect("#1328 unsupported is a normal result, not CAPABILITY_UNAVAILABLE");
    negative_assert_eq!(
        iso_keys(&result),
        vec![
            "requested".to_string(),
            "requestedAt".to_string(),
            "reason".to_string()
        ],
        "#1328 unsupported shape field order"
    );
    negative_assert_eq!(
        result.get("requested"),
        Some(&JsValue::Bool(false)),
        "#1328 unsupported shape requested flag"
    );
    negative_assert_eq!(
        result.get("reason").and_then(JsValue::as_string),
        Some(&pilotdeck_protocol::JsString::from("unsupported")),
        "#1328 unsupported shape reason"
    );
    let requested_at = result
        .get("requestedAt")
        .and_then(JsValue::as_string)
        .expect("#1328 unsupported shape carries requestedAt");
    negative_assert!(
        parse_iso(requested_at).is_some(),
        "#1328 requestedAt is a fresh ISO timestamp"
    );
    // A repeat call is not cached: a fresh object with a fresh timestamp each
    // time, exactly like the TS guard re-entering on every request.
    let second = requests
        .request("prepare_weixin_login".into(), crate::metadata::object([]))
        .await
        .expect("#1328 repeat call stays Ok");
    negative_assert_eq!(
        second.get("requested"),
        Some(&JsValue::Bool(false)),
        "#1328 repeat call re-enters the guard"
    );
    let second_at = second
        .get("requestedAt")
        .and_then(JsValue::as_string)
        .expect("#1328 repeat call carries requestedAt");
    negative_assert!(
        parse_iso(second_at).is_some(),
        "#1328 repeat call timestamp is ISO"
    );
    requests
        .owner
        .shutdown()
        .await
        .expect("#1328 fixture owner shutdown");
    requests
        .project
        .dispose()
        .await
        .expect("#1328 fixture project shutdown");
}

#[tokio::test]
async fn prepare_weixin_login_forwards_the_callback_result_each_call() {
    use crate::runtime::turn::gateway::GatewayRequests;
    use std::sync::atomic::{AtomicU32, Ordering};

    let (_scratch, mut requests) = std::panic::catch_unwind(super::super::management_tests::setup)
        .expect("#1328 callback fixture setup");
    let calls = std::sync::Arc::new(AtomicU32::new(0));
    let counter = calls.clone();
    requests.prepare_weixin_login = Some(std::sync::Arc::new(move || {
        let counter = counter.clone();
        Box::pin(async move {
            let n = counter.fetch_add(1, Ordering::SeqCst) + 1;
            // The CLI callback shape: requestedAt captured before the work,
            // `{requested: true, requestedAt}` after it (pilotdeck.ts:240-247).
            Ok(crate::metadata::object([
                ("requested", JsValue::Bool(true)),
                (
                    "requestedAt",
                    JsValue::String(pilotdeck_protocol::JsString::from(format!(
                        "2026-11-05T00:00:0{n}.000Z"
                    ))),
                ),
                (
                    "attempt",
                    JsValue::String(pilotdeck_protocol::JsString::from(format!("{n}"))),
                ),
            ]))
        })
    }));
    for attempt in 1..=2u32 {
        let result = requests
            .request("prepare_weixin_login".into(), crate::metadata::object([]))
            .await
            .expect("#1328 callback result is forwarded as-is");
        negative_assert_eq!(
            result.get("requested"),
            Some(&JsValue::Bool(true)),
            "#1328 callback result requested flag"
        );
        negative_assert_eq!(
            result.get("requestedAt").and_then(JsValue::as_string),
            Some(&pilotdeck_protocol::JsString::from(format!(
                "2026-11-05T00:00:0{attempt}.000Z"
            ))),
            "#1328 callback result requestedAt is forwarded verbatim"
        );
        negative_assert_eq!(
            result.get("attempt").and_then(JsValue::as_string),
            Some(&pilotdeck_protocol::JsString::from(format!("{attempt}"))),
            "#1328 callback runs once per request, no caching"
        );
    }
    negative_assert_eq!(
        calls.load(Ordering::SeqCst),
        2,
        "#1328 repeat delivery re-invokes the callback"
    );
    // Removing the callback again (setPrepareWeixinLogin(undefined)) returns
    // the unsupported shape rather than replaying the old callback.
    requests.prepare_weixin_login = None;
    let result = requests
        .request("prepare_weixin_login".into(), crate::metadata::object([]))
        .await
        .expect("#1328 cleared callback stays Ok");
    negative_assert_eq!(
        result.get("reason").and_then(JsValue::as_string),
        Some(&pilotdeck_protocol::JsString::from("unsupported")),
        "#1328 cleared callback falls back to unsupported"
    );
    negative_assert_eq!(
        calls.load(Ordering::SeqCst),
        2,
        "#1328 cleared callback is not invoked again"
    );
    requests
        .owner
        .shutdown()
        .await
        .expect("#1328 callback fixture owner shutdown");
    requests
        .project
        .dispose()
        .await
        .expect("#1328 callback fixture project shutdown");
}

// #1442/#1599: user_dialog respond must not run any host-supplied callback
// (the on_change observer, the accepts validator, the settle hook, or the
// resolver) while the GatewayUserDialogBus MutexGuard is alive. Every probe
// below captures the bus Arc and records whether try_lock re-acquires it;
// a callback executed under the guard records WouldBlock and fails.

/// Recorded try_lock outcome of one host callback: (callback, re-entered).
type ReentryLog1442 = std::sync::Arc<std::sync::Mutex<Vec<(&'static str, bool)>>>;

fn assert_lock_free_1442(log: &ReentryLog1442, expected: &[&'static str]) {
    let recorded = log.lock().unwrap().clone();
    // Guard sensitivity first: a callback that ran under the guard must report
    // *that*, even when the mutant also shifts the probe sequence.
    for (name, reentered) in &recorded {
        negative_assert!(
            *reentered,
            "#1442 the {name} callback re-acquired the bus lock; it must not run under the guard"
        );
    }
    let names: Vec<&'static str> = recorded.iter().map(|(name, _)| *name).collect();
    negative_assert_eq!(
        names.as_slice(),
        expected,
        "#1442 every probe fires exactly once, in order"
    );
}

/// Wires a bus whose observer records try_lock re-acquisition (#1442).
fn wire_reentry_bus_1442(requests: &mut super::super::requests::Requests, log: &ReentryLog1442) {
    let log = log.clone();
    requests.user_dialog_bus = std::sync::Arc::new_cyclic(
        |bus: &std::sync::Weak<std::sync::Mutex<crate::user_dialog::GatewayUserDialogBus>>| {
            let bus = bus.clone();
            let log = log.clone();
            std::sync::Mutex::new(crate::user_dialog::GatewayUserDialogBus::new(
                crate::user_dialog::DialogRuntime::default(),
                Some(Box::new(move |_event| {
                    let reentered = bus.upgrade().is_some_and(|bus| bus.try_lock().is_ok());
                    log.lock().unwrap().push(("observer", reentered));
                    Ok(())
                })),
            ))
        },
    );
}

/// Registers a dialog whose on_settled/resolve callbacks record try_lock
/// re-acquisition (#1442).
fn register_reentry_dialog_1442(
    requests: &super::super::requests::Requests,
    session: &JsString,
    request_id: &str,
    log: &ReentryLog1442,
) {
    let settle_bus = requests.user_dialog_bus.clone();
    let settle_log = log.clone();
    let resolve_bus = requests.user_dialog_bus.clone();
    let resolve_log = log.clone();
    requests
        .user_dialog_bus
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .register(
            session,
            crate::user_dialog::PendingDialog {
                request_id: request_id.into(),
                dialog_kind: "input".into(),
                tool_call_id: "call-1442".into(),
                tool_name: "request_user_input".into(),
                event: crate::metadata::object([("requestId", JsValue::String(request_id.into()))]),
                accepts: Arc::new(|_value| true),
                resolve: Box::new(move |_answer| {
                    let reentered = resolve_bus.try_lock().is_ok();
                    resolve_log.lock().unwrap().push(("resolve", reentered));
                }),
                reject: Box::new(|_error| {}),
                on_settled: Some(Box::new(move || {
                    let reentered = settle_bus.try_lock().is_ok();
                    settle_log.lock().unwrap().push(("on_settled", reentered));
                    Ok(())
                })),
            },
        );
}

// #1442 a: the on_change observer must not run under the bus guard. The
// registration broadcast fires the observer while the test itself holds the
// lock; the respond broadcast fires it under the method layer's guard. The
// dialog itself is inert: this probe is about the observer only.
#[tokio::test]
async fn user_dialog_observer_reenters_the_bus_lock_free() {
    let (_scratch, mut requests) = std::panic::catch_unwind(super::super::management_tests::setup)
        .expect("#1442 observer fixture setup");
    let log: ReentryLog1442 = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    wire_reentry_bus_1442(&mut requests, &log);
    let session: JsString = "1442-observer-session".into();
    let resolved: std::sync::Arc<std::sync::Mutex<Vec<crate::user_dialog::DialogAnswer>>> =
        std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    register_dialog_1437(&requests, &session, "1442-observer-dialog", &resolved);
    // #1442: the host drains the registration broadcast after dropping its
    // own guard — the same caller contract the channel's begin() follows.
    // The guard is bound to its own statement; delivering on the same chain
    // would keep it alive across the observer.
    let outbox = requests
        .user_dialog_bus
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .drain_events();
    outbox.deliver();
    let result = requests
        .user_dialog_request(
            "user_dialog_respond".into(),
            crate::metadata::object([
                ("sessionKey", JsValue::String(session.clone())),
                ("requestId", "1442-observer-dialog".into()),
                (
                    "result",
                    crate::metadata::object([
                        ("behavior", "cancelled".into()),
                        ("reason", "done".into()),
                    ]),
                ),
            ]),
        )
        .await
        .expect("#1442 observer probe respond delivers");
    negative_assert_eq!(
        result.get("delivered"),
        Some(&JsValue::Bool(true)),
        "#1442 observer probe respond reports delivered"
    );
    assert_lock_free_1442(&log, &["observer", "observer"]);
    requests
        .owner
        .shutdown()
        .await
        .expect("#1442 observer fixture owner shutdown");
    requests
        .project
        .dispose()
        .await
        .expect("#1442 observer fixture project shutdown");
}

// #1442 b: the settle hook and the resolver must not run under the bus guard.
#[tokio::test]
async fn user_dialog_settle_and_resolve_reenter_the_bus_lock_free() {
    let (_scratch, mut requests) = std::panic::catch_unwind(super::super::management_tests::setup)
        .expect("#1442 settle fixture setup");
    let log: ReentryLog1442 = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    wire_user_dialog_bus_1437(&mut requests);
    let session: JsString = "1442-settle-session".into();
    register_reentry_dialog_1442(&requests, &session, "1442-settle-dialog", &log);
    let result = requests
        .user_dialog_request(
            "user_dialog_respond".into(),
            crate::metadata::object([
                ("sessionKey", JsValue::String(session.clone())),
                ("requestId", "1442-settle-dialog".into()),
                (
                    "result",
                    crate::metadata::object([
                        ("behavior", "answered".into()),
                        ("value", "yes".into()),
                    ]),
                ),
            ]),
        )
        .await
        .expect("#1442 settle probe respond delivers");
    negative_assert_eq!(
        result.get("delivered"),
        Some(&JsValue::Bool(true)),
        "#1442 settle probe respond reports delivered"
    );
    assert_lock_free_1442(&log, &["on_settled", "resolve"]);
    requests
        .owner
        .shutdown()
        .await
        .expect("#1442 settle fixture owner shutdown");
    requests
        .project
        .dispose()
        .await
        .expect("#1442 settle fixture project shutdown");
}

// #1442 c: the accepts validator must not run under the bus guard.
#[tokio::test]
async fn user_dialog_accepts_reenters_the_bus_lock_free() {
    let (_scratch, mut requests) = std::panic::catch_unwind(super::super::management_tests::setup)
        .expect("#1442 accepts fixture setup");
    let log: ReentryLog1442 = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    wire_user_dialog_bus_1437(&mut requests);
    let session: JsString = "1442-accepts-session".into();
    let accepts_bus = requests.user_dialog_bus.clone();
    let accepts_log = log.clone();
    requests
        .user_dialog_bus
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .register(
            &session,
            crate::user_dialog::PendingDialog {
                request_id: "1442-accepts-dialog".into(),
                dialog_kind: "input".into(),
                tool_call_id: "call-1442".into(),
                tool_name: "request_user_input".into(),
                event: crate::metadata::object([(
                    "requestId",
                    JsValue::String("1442-accepts-dialog".into()),
                )]),
                accepts: Arc::new(move |value| {
                    let reentered = accepts_bus.try_lock().is_ok();
                    accepts_log.lock().unwrap().push(("accepts", reentered));
                    value.as_string().is_some()
                }),
                resolve: Box::new(|_answer| {}),
                reject: Box::new(|_error| {}),
                on_settled: None,
            },
        );
    let result = requests
        .user_dialog_request(
            "user_dialog_respond".into(),
            crate::metadata::object([
                ("sessionKey", JsValue::String(session.clone())),
                ("requestId", "1442-accepts-dialog".into()),
                (
                    "result",
                    crate::metadata::object([
                        ("behavior", "answered".into()),
                        ("value", "yes".into()),
                    ]),
                ),
            ]),
        )
        .await
        .expect("#1442 accepts probe respond delivers");
    negative_assert_eq!(
        result.get("delivered"),
        Some(&JsValue::Bool(true)),
        "#1442 accepts probe respond reports delivered"
    );
    assert_lock_free_1442(&log, &["accepts"]);
    requests
        .owner
        .shutdown()
        .await
        .expect("#1442 accepts fixture owner shutdown");
    requests
        .project
        .dispose()
        .await
        .expect("#1442 accepts fixture project shutdown");
}

// #1442: a genuine re-entrant bus call (lock + list) issued from inside the
// observer callback, bounded everywhere: the re-entrant acquisition runs on
// its own thread and the callback waits for it with a hard recv_timeout, so
// even a callback executed under the guard cannot hang the test — the
// re-entrant call then merely misses its deadline and turns this red.
#[tokio::test]
async fn user_dialog_observer_reentry_completes_within_timeout() {
    let (_scratch, mut requests) = std::panic::catch_unwind(super::super::management_tests::setup)
        .expect("#1442 reentry fixture setup");
    let log: ReentryLog1442 = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let session: JsString = "1442-reentry-session".into();
    let session_key = session.clone();
    requests.user_dialog_bus = std::sync::Arc::new_cyclic(
        |bus: &std::sync::Weak<std::sync::Mutex<crate::user_dialog::GatewayUserDialogBus>>| {
            let bus = bus.clone();
            let log = log.clone();
            std::sync::Mutex::new(crate::user_dialog::GatewayUserDialogBus::new(
                crate::user_dialog::DialogRuntime::default(),
                Some(Box::new(move |_event| {
                    // A real re-entrant bus call: lock() + list(). It runs on
                    // its own thread so a guard held across this callback
                    // misses the deadline instead of deadlocking the host.
                    let (done, arrived) = std::sync::mpsc::channel::<usize>();
                    let reentry = bus.upgrade().expect("#1442 bus arc alive");
                    let reentry_session = session_key.clone();
                    std::thread::spawn(move || {
                        let dialogs = reentry
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .list(&reentry_session);
                        let _ = done.send(dialogs.len());
                    });
                    let reentered = arrived
                        .recv_timeout(std::time::Duration::from_secs(2))
                        .is_ok();
                    log.lock().unwrap().push(("observer reentry", reentered));
                    Ok(())
                })),
            ))
        },
    );
    register_reentry_dialog_1442(&requests, &session, "1442-reentry-dialog", &log);
    // The reentry dialog's settle/resolve try_lock probes are covered above;
    // here they additionally record whether the guard is held on the
    // registration and respond paths. The registration broadcast is drained
    // after the registering guard drops (#1442 caller contract); the guard
    // must not stay alive across the deliver.
    let outbox = requests
        .user_dialog_bus
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .drain_events();
    outbox.deliver();
    let result = requests
        .user_dialog_request(
            "user_dialog_respond".into(),
            crate::metadata::object([
                ("sessionKey", JsValue::String(session.clone())),
                ("requestId", "1442-reentry-dialog".into()),
                (
                    "result",
                    crate::metadata::object([("behavior", "cancelled".into())]),
                ),
            ]),
        )
        .await
        .expect("#1442 reentry probe respond delivers");
    negative_assert_eq!(
        result.get("delivered"),
        Some(&JsValue::Bool(true)),
        "#1442 reentry probe respond reports delivered"
    );
    let recorded = log.lock().unwrap().clone();
    let names: Vec<&'static str> = recorded.iter().map(|(name, _)| *name).collect();
    negative_assert_eq!(
        names.as_slice(),
        &[
            "observer reentry",
            "on_settled",
            "observer reentry",
            "resolve"
        ],
        "#1442 the registration broadcast, settle, respond broadcast and resolver all fire"
    );
    for (name, reentered) in recorded {
        negative_assert!(
            reentered,
            "#1442 the {name} callback's re-entrant bus call completed within the timeout"
        );
    }
    requests
        .owner
        .shutdown()
        .await
        .expect("#1442 reentry fixture owner shutdown");
    requests
        .project
        .dispose()
        .await
        .expect("#1442 reentry fixture project shutdown");
}

// #1599: when the peek finds no pending dialog, an answered respond must not
// consume whatever a host callback registers under the same request id during
// the observer delivery that sits between the snapshot and the consume. TS
// peeks once and delivers nothing when the peek is empty
// (InProcessGateway.ts:2668-2674); consuming the late registration would hand
// it an answer its own validator never saw.
#[tokio::test]
async fn user_dialog_respond_without_a_snapshot_leaves_a_late_registration_pending() {
    let (_scratch, mut requests) = std::panic::catch_unwind(super::super::management_tests::setup)
        .expect("#1599 late-registration fixture setup");
    let session: JsString = "1599-late-session".into();
    let target: JsString = "1599-late-dialog".into();
    let late_resolved = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    requests.user_dialog_bus = std::sync::Arc::new_cyclic(
        |bus: &std::sync::Weak<std::sync::Mutex<crate::user_dialog::GatewayUserDialogBus>>| {
            let bus = bus.clone();
            let session = session.clone();
            let target = target.clone();
            let late_resolved = late_resolved.clone();
            std::sync::Mutex::new(crate::user_dialog::GatewayUserDialogBus::new(
                crate::user_dialog::DialogRuntime::default(),
                Some(Box::new(move |_event| {
                    let Some(bus) = bus.upgrade() else {
                        return Ok(());
                    };
                    let mut bus = bus.lock().unwrap_or_else(|e| e.into_inner());
                    if bus.has_pending(&session, &target) {
                        return Ok(());
                    }
                    let late_resolved = late_resolved.clone();
                    bus.register(
                        &session,
                        crate::user_dialog::PendingDialog {
                            request_id: target.clone(),
                            dialog_kind: "input".into(),
                            tool_call_id: "call-late".into(),
                            tool_name: "request_user_input".into(),
                            event: crate::metadata::object([(
                                "requestId",
                                JsValue::String(target.clone()),
                            )]),
                            // Rejects every value: the answer must never reach
                            // this dialog, validated or not.
                            accepts: Arc::new(|_value| false),
                            resolve: Box::new(move |_answer| {
                                late_resolved.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                            }),
                            reject: Box::new(|_error| {}),
                            on_settled: None,
                        },
                    );
                    Ok(())
                })),
            ))
        },
    );
    // Queue one broadcast without delivering it: the next respond drains it and
    // runs the observer between its snapshot and its consume.
    register_dialog_1437(
        &requests,
        &session,
        "1599-trigger-dialog",
        &std::sync::Arc::new(std::sync::Mutex::new(Vec::new())),
    );
    let result = requests
        .user_dialog_request(
            "user_dialog_respond".into(),
            crate::metadata::object([
                ("sessionKey", JsValue::String(session.clone())),
                ("requestId", JsValue::String(target.clone())),
                (
                    "result",
                    crate::metadata::object([
                        ("behavior", "answered".into()),
                        ("value", "late".into()),
                    ]),
                ),
            ]),
        )
        .await
        .expect("#1599 late-registration respond returns");
    negative_assert_eq!(
        result.get("delivered"),
        Some(&JsValue::Bool(false)),
        "#1599 an answered respond with no peeked dialog must report not delivered"
    );
    negative_assert_eq!(
        late_resolved.load(std::sync::atomic::Ordering::SeqCst),
        0,
        "#1599 the late registration must not receive an answer it never validated"
    );
    negative_assert!(
        requests
            .user_dialog_bus
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .has_pending(&session, &target),
        "#1599 the late registration stays pending for its own answer"
    );
    requests
        .owner
        .shutdown()
        .await
        .expect("#1599 late-registration fixture owner shutdown");
    requests
        .project
        .dispose()
        .await
        .expect("#1599 late-registration fixture project shutdown");
}

// #1599: the accepts validator is host code and runs outside the bus guard,
// so it can reenter and register a new dialog under the same request id. The
// answer was validated against the old registration and must not be delivered
// to the new one: respond reports `delivered: false` and the replacement stays
// pending for its own answer.
#[tokio::test]
async fn user_dialog_respond_reports_not_delivered_when_accepts_replaces_the_dialog() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    let (_scratch, mut requests) = std::panic::catch_unwind(super::super::management_tests::setup)
        .expect("#1599 accepts-replacement fixture setup");
    let session: JsString = "1599-accepts-session".into();
    let target: JsString = "1599-accepts-dialog".into();
    let replacement_resolved = Arc::new(AtomicUsize::new(0));
    requests.user_dialog_bus = std::sync::Arc::new_cyclic(
        |bus: &std::sync::Weak<std::sync::Mutex<crate::user_dialog::GatewayUserDialogBus>>| {
            let mut dialogs = crate::user_dialog::GatewayUserDialogBus::connected(
                crate::user_dialog::DialogRuntime::default(),
            );
            let weak = bus.clone();
            let session_for_accepts = session.clone();
            let target_for_accepts = target.clone();
            let resolved_for_replacement = replacement_resolved.clone();
            dialogs.register(
                &session,
                crate::user_dialog::PendingDialog {
                    request_id: target.clone(),
                    dialog_kind: "input".into(),
                    tool_call_id: "call-original".into(),
                    tool_name: "request_user_input".into(),
                    event: crate::metadata::object([(
                        "requestId",
                        JsValue::String(target.clone()),
                    )]),
                    // Accepts the value, but first reenters the bus and
                    // registers a replacement under the same request id.
                    accepts: Arc::new(move |_value| {
                        if let Some(shared) = weak.upgrade() {
                            let _guard = shared.lock().unwrap_or_else(|e| e.into_inner());
                            let mut shared = _guard;
                            let replacement_resolved = resolved_for_replacement.clone();
                            shared.register(
                                &session_for_accepts,
                                crate::user_dialog::PendingDialog {
                                    request_id: target_for_accepts.clone(),
                                    dialog_kind: "input".into(),
                                    tool_call_id: "call-replacement".into(),
                                    tool_name: "request_user_input".into(),
                                    event: crate::metadata::object([(
                                        "requestId",
                                        JsValue::String(target_for_accepts.clone()),
                                    )]),
                                    accepts: Arc::new(|_value| false),
                                    resolve: Box::new(move |_answer| {
                                        replacement_resolved.fetch_add(1, Ordering::SeqCst);
                                    }),
                                    reject: Box::new(|_error| {}),
                                    on_settled: None,
                                },
                            );
                        }
                        true
                    }),
                    resolve: Box::new(|_answer| {}),
                    reject: Box::new(|_error| {}),
                    on_settled: None,
                },
            );
            std::sync::Mutex::new(dialogs)
        },
    );
    let result = requests
        .user_dialog_request(
            "user_dialog_respond".into(),
            crate::metadata::object([
                ("sessionKey", JsValue::String(session.clone())),
                ("requestId", JsValue::String(target.clone())),
                (
                    "result",
                    crate::metadata::object([
                        ("behavior", "answered".into()),
                        ("value", "accepted".into()),
                    ]),
                ),
            ]),
        )
        .await
        .expect("#1599 accepts-replacement respond returns");
    negative_assert_eq!(
        result.get("delivered"),
        Some(&JsValue::Bool(false)),
        "#1599 an answer validated against a replaced dialog must not be delivered"
    );
    negative_assert_eq!(
        replacement_resolved.load(Ordering::SeqCst),
        0,
        "#1599 the replacement must not receive an answer it never validated"
    );
    negative_assert!(
        requests
            .user_dialog_bus
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .has_pending(&session, &target),
        "#1599 the replacement stays pending for its own answer"
    );
    requests
        .owner
        .shutdown()
        .await
        .expect("#1599 accepts-replacement fixture owner shutdown");
    requests
        .project
        .dispose()
        .await
        .expect("#1599 accepts-replacement fixture project shutdown");
}

// #1599: a lease that expired during the consume is a real, already-happened
// broadcast. A failing settle hook rolls back only this consume's own
// "settled" event (TS remove() throws before it mutates,
// GatewayUserDialogBus.ts:219-226) — the renderer still has to hear that the
// lease expired, otherwise its claimed state never clears.
#[tokio::test]
async fn user_dialog_respond_settle_failure_still_delivers_the_expired_lease() {
    use std::sync::atomic::{AtomicI64, Ordering};

    let (_scratch, mut requests) = std::panic::catch_unwind(super::super::management_tests::setup)
        .expect("#1599 settle-failure fixture setup");
    let session: JsString = "1599-settle-session".into();
    let request: JsString = "1599-settle-dialog".into();
    let clock = Arc::new(AtomicI64::new(1_000));
    let seen: Arc<std::sync::Mutex<Vec<(String, String)>>> = Arc::default();
    let seen_by_observer = seen.clone();
    let mut bus = crate::user_dialog::GatewayUserDialogBus::new(
        crate::user_dialog::DialogRuntime {
            now: {
                let clock = clock.clone();
                Arc::new(move || clock.load(Ordering::SeqCst))
            },
            uuid: Arc::new(|| "1599-settle-lease".into()),
        },
        Some(Box::new(move |event| {
            let read = |key: &str| {
                event
                    .get(key)
                    .and_then(JsValue::as_string)
                    .and_then(|value| value.to_utf8().ok())
                    .unwrap_or_default()
            };
            seen_by_observer
                .lock()
                .unwrap()
                .push((read("type"), read("action")));
            Ok(())
        })),
    );
    bus.register(
        &session,
        crate::user_dialog::PendingDialog {
            request_id: request.clone(),
            dialog_kind: "input".into(),
            tool_call_id: "call-settle".into(),
            tool_name: "request_user_input".into(),
            event: crate::metadata::object([("requestId", JsValue::String(request.clone()))]),
            accepts: Arc::new(|_value| true),
            resolve: Box::new(|_answer| {}),
            reject: Box::new(|_error| {}),
            on_settled: Some(Box::new(|| {
                Err(crate::user_dialog::DialogError::Invalid(
                    "journal-io".into(),
                ))
            })),
        },
    );
    requests.user_dialog_bus = std::sync::Arc::new(std::sync::Mutex::new(bus));
    let claimed = requests
        .user_dialog_request(
            "user_dialog_claim".into(),
            crate::metadata::object([
                ("sessionKey", JsValue::String(session.clone())),
                ("requestId", JsValue::String(request.clone())),
                ("ttlMs", JsValue::Number(1_000.0)),
            ]),
        )
        .await
        .expect("#1599 settle fixture claim");
    negative_assert_eq!(
        claimed.get("claimed"),
        Some(&JsValue::Bool(true)),
        "#1599 settle fixture claim succeeds"
    );
    let lease_id = claimed
        .get("leaseId")
        .cloned()
        .unwrap_or(JsValue::Undefined);
    // Drop the renderer-visible claim broadcast; only the expiry matters here.
    seen.lock().unwrap().clear();
    clock.store(2_500, Ordering::SeqCst);
    let result = requests
        .user_dialog_request(
            "user_dialog_respond".into(),
            crate::metadata::object([
                ("sessionKey", JsValue::String(session.clone())),
                ("requestId", JsValue::String(request.clone())),
                ("leaseId", lease_id),
                (
                    "result",
                    crate::metadata::object([
                        ("behavior", "answered".into()),
                        ("value", "answer".into()),
                    ]),
                ),
            ]),
        )
        .await;
    negative_assert!(
        result.is_err(),
        "#1599 the failing settle hook fails the respond"
    );
    let events = seen.lock().unwrap().clone();
    negative_assert!(
        events
            .iter()
            .any(|(kind, action)| kind == "lease_changed" && action == "expired"),
        "#1599 a failing settle hook still delivers the lease expiry it did not roll back"
    );
    negative_assert!(
        !events.iter().any(|(kind, _)| kind == "settled"),
        "#1599 the rolled-back settled broadcast is not delivered"
    );
    requests
        .owner
        .shutdown()
        .await
        .expect("#1599 settle fixture owner shutdown");
    requests
        .project
        .dispose()
        .await
        .expect("#1599 settle fixture project shutdown");
}

/// #1441 五方法分发（wired 态）：upload_create/get/part/complete/cancel 经
/// `Requests::request`（admission → methods::request → upload family →
/// spawn_blocking）打到真实 `UploadStore`，生命周期全链（探针
/// ts-probe/p3-lease-lifecycle.out.txt 的 Rust 镜像）。
#[tokio::test]
async fn upload_methods_dispatch_to_the_wired_store() {
    use crate::runtime::turn::gateway::GatewayRequests;

    let (scratch, requests) = std::panic::catch_unwind(super::super::management_tests::setup)
        .expect("#1441 registry fixture setup");
    let project = requests.options.project.clone();
    let project_key = project.to_string_lossy().into_owned();
    let owned = project.clone();
    let listed = project.clone();
    let store = Arc::new(crate::upload::UploadStore::new(
        crate::upload::UploadStoreOptions::new(
            // #1441 (fix-r1, simplify#1): the production wiring closure —
            // the dispatch test must not carry a hand copy of the
            // dual-spelling rule that can drift from the real one.
            move |key: &pilotdeck_protocol::JsString| {
                super::super::assembly::upload_resolve_project(&owned, key)
            },
            move || Ok(vec![listed.clone()]),
        ),
    ));
    let wired = super::super::requests::Requests {
        upload_gateway: Arc::new(crate::upload::gateway::UploadGateway::new(store.clone())),
        ..requests
    };
    let manifest = JsValue::Array(vec![crate::metadata::object([
        ("clientFileId", "f1".into()),
        ("name", "a.txt".into()),
        ("relativePath", "a.txt".into()),
        ("size", JsValue::Number(3.0)),
    ])]);
    // upload_create
    let created = wired
        .request(
            "upload_create".into(),
            crate::metadata::object([
                ("projectKey", JsValue::from(project_key.as_str())),
                ("files", manifest),
            ]),
        )
        .await
        .expect("#1441 upload_create must dispatch to the wired store");
    let upload_id = created
        .get("uploadId")
        .and_then(JsValue::as_string)
        .and_then(|s| s.to_utf8().ok())
        .expect("#1441 the created record carries uploadId");
    negative_assert_eq!(
        created.get("status").and_then(JsValue::as_string),
        Some(&pilotdeck_protocol::JsString::from("created")),
        "#1441 the created record reports created"
    );
    // upload_part
    let part = wired
        .request(
            "upload_part".into(),
            crate::metadata::object([
                ("uploadId", JsValue::from(upload_id.as_str())),
                ("clientFileId", "f1".into()),
                ("contentBase64", "YWJj".into()),
            ]),
        )
        .await
        .expect("#1441 upload_part must dispatch to the wired store");
    negative_assert_eq!(
        part.get("bytes").and_then(JsValue::as_number),
        Some(3.0),
        "#1441 the part response reports the decoded byte count"
    );
    // upload_get
    let got = wired
        .request(
            "upload_get".into(),
            crate::metadata::object([("uploadId", JsValue::from(upload_id.as_str()))]),
        )
        .await
        .expect("#1441 upload_get must dispatch to the wired store");
    negative_assert_eq!(
        got.get("uploadedBytes").and_then(JsValue::as_number),
        Some(3.0),
        "#1441 the fetched record reports the uploaded bytes"
    );
    // upload_complete
    let done = wired
        .request(
            "upload_complete".into(),
            crate::metadata::object([("uploadId", JsValue::from(upload_id.as_str()))]),
        )
        .await
        .expect("#1441 upload_complete must dispatch to the wired store");
    negative_assert_eq!(
        done.get("status").and_then(JsValue::as_string),
        Some(&pilotdeck_protocol::JsString::from("completed")),
        "#1441 the completed record reports completed"
    );
    // upload_cancel on completed: the TS refusal crosses the whole chain
    let refused = wired
        .request(
            "upload_cancel".into(),
            crate::metadata::object([("uploadId", JsValue::from(upload_id.as_str()))]),
        )
        .await
        .expect_err("#1441 a completed upload must refuse cancel");
    negative_assert_eq!(
        refused.code,
        "UPLOAD_ALREADY_COMPLETED",
        "#1441 the completed-cancel refusal keeps its code"
    );
    // upload_cancel on an in-gate upload: cancelled + files removed
    let second = wired
        .request(
            "upload_create".into(),
            crate::metadata::object([
                ("projectKey", JsValue::from(project_key.as_str())),
                (
                    "files",
                    JsValue::Array(vec![crate::metadata::object([
                        ("clientFileId", "f1".into()),
                        ("name", "b.txt".into()),
                        ("relativePath", "b.txt".into()),
                        ("size", JsValue::Number(3.0)),
                    ])]),
                ),
            ]),
        )
        .await
        .expect("#1441 the second create must dispatch");
    let second_id = second
        .get("uploadId")
        .and_then(JsValue::as_string)
        .and_then(|s| s.to_utf8().ok())
        .expect("#1441 the second record carries uploadId");
    let cancelled = wired
        .request(
            "upload_cancel".into(),
            crate::metadata::object([("uploadId", JsValue::from(second_id.as_str()))]),
        )
        .await
        .expect("#1441 upload_cancel must dispatch to the wired store");
    negative_assert_eq!(
        cancelled.get("status").and_then(JsValue::as_string),
        Some(&pilotdeck_protocol::JsString::from("cancelled")),
        "#1441 the cancelled record reports cancelled"
    );
    wired
        .owner
        .shutdown()
        .await
        .expect("#1441 wired fixture owner shutdown");
    wired
        .project
        .dispose()
        .await
        .expect("#1441 wired fixture project shutdown");
    drop(scratch);
}

/// #1441 五方法分发（unavailable 态）：宿主未接线 `uploadLifecycle`
/// （`UploadGateway::unavailable()`）时五个方法一律 CAPABILITY_UNAVAILABLE
/// "Gateway uploads are unavailable."——与 wired 态互为反差。
#[tokio::test]
async fn upload_methods_refuse_when_unwired() {
    use crate::runtime::turn::gateway::GatewayRequests;

    let (_scratch, requests) = std::panic::catch_unwind(super::super::management_tests::setup)
        .expect("#1441 registry fixture setup");
    for method in [
        "upload_create",
        "upload_get",
        "upload_part",
        "upload_complete",
        "upload_cancel",
    ] {
        // upload_part 校验参数在能力门之前，所以它要带全合法参数才能
        // 穿过参数层、停在能力门；其余四门能力门在前，空参数即可。
        let params = if method == "upload_part" {
            crate::metadata::object([
                ("uploadId", "01890a7e-4d8a-7b1e-9d20-64b9f2a5a1a1".into()),
                ("clientFileId", "f1".into()),
                ("contentBase64", "YWJj".into()),
            ])
        } else {
            crate::metadata::object([(
                "uploadId",
                JsValue::from("01890a7e-4d8a-7b1e-9d20-64b9f2a5a1a1"),
            )])
        };
        let error = requests
            .request(method.into(), params)
            .await
            .expect_err("#1441 every upload method must refuse when unwired");
        negative_assert_eq!(
            error.code,
            "CAPABILITY_UNAVAILABLE",
            "#1441 {method} must report the capability code when unwired"
        );
        negative_assert_eq!(
            String::from_utf16_lossy(error.message.units()),
            "Gateway uploads are unavailable.",
            "#1441 {method} must keep the TS message when unwired"
        );
    }
    requests
        .owner
        .shutdown()
        .await
        .expect("#1441 unwired fixture owner shutdown");
    requests
        .project
        .dispose()
        .await
        .expect("#1441 unwired fixture project shutdown");
}

/// #1441 参数序反差：`upload_part` 校验参数在能力门之前（gateway.rs
/// :156-161——uploadId/clientFileId/base64 都过了才 require()），而
/// create/get/complete/cancel 的能力门在参数之前。unavailable 态钉序：
/// part 传坏参数要报参数错，create 缺 projectKey 仍报能力错。
#[tokio::test]
async fn upload_part_validates_params_before_the_capability_gate() {
    use crate::runtime::turn::gateway::GatewayRequests;

    let (_scratch, requests) = std::panic::catch_unwind(super::super::management_tests::setup)
        .expect("#1441 registry fixture setup");
    // part：坏 base64 → UPLOAD_PART_INVALID（不是 CAPABILITY_UNAVAILABLE）
    let error = requests
        .request(
            "upload_part".into(),
            crate::metadata::object([
                ("uploadId", "01890a7e-4d8a-7b1e-9d20-64b9f2a5a1a1".into()),
                ("clientFileId", "f1".into()),
                ("contentBase64", "!!!".into()),
            ]),
        )
        .await
        .expect_err("#1441 an invalid part payload must fail on the parameter, not the capability");
    negative_assert_eq!(
        error.code,
        "UPLOAD_PART_INVALID",
        "#1441 upload_part validates contentBase64 before requiring the provider"
    );
    // part：缺 clientFileId → UPLOAD_MANIFEST_MISMATCH（同样先于能力门）
    let error = requests
        .request(
            "upload_part".into(),
            crate::metadata::object([
                ("uploadId", "01890a7e-4d8a-7b1e-9d20-64b9f2a5a1a1".into()),
                ("contentBase64", "YWJj".into()),
            ]),
        )
        .await
        .expect_err("#1441 a missing clientFileId must fail on the parameter");
    negative_assert_eq!(
        error.code,
        "UPLOAD_MANIFEST_MISMATCH",
        "#1441 upload_part validates clientFileId before requiring the provider"
    );
    // create：缺 projectKey → CAPABILITY_UNAVAILABLE（能力门在参数前）
    let error = requests
        .request("upload_create".into(), crate::metadata::object([]))
        .await
        .expect_err("#1441 a parameterless create must fail on the capability first");
    negative_assert_eq!(
        error.code,
        "CAPABILITY_UNAVAILABLE",
        "#1441 upload_create requires the provider before validating the manifest"
    );
    requests
        .owner
        .shutdown()
        .await
        .expect("#1441 order fixture owner shutdown");
    requests
        .project
        .dispose()
        .await
        .expect("#1441 order fixture project shutdown");
}

// ---------------------------------------------------------------------------
// #1604 S2: the run / steer family reads its inputs exactly like the TS
// property reads — TS has no input guard on these methods
// (InProcessGateway.ts:1859-1893, 2479-2553). Every expectation below was
// probed first (`_tmp/20261026-issue-1604/ts-probe/output.txt` s/c/h/r/a
// cells; `output2.txt` q cells with the real FileRunRegistry, Node v22.22.0).

#[tokio::test]
async fn steer_family_dropped_guards_follow_the_ts_lookup_semantics() {
    use crate::runtime::turn::gateway::GatewayRequests;

    let (_scratch, requests) = std::panic::catch_unwind(super::super::management_tests::setup)
        .expect("#1604 registry fixture setup");
    // Nullish params: the `input.sessionKey` read itself throws (probes s9,
    // c5) and the boundary serializes the TypeError with its own code.
    for (params, reading) in [(JsValue::Null, "null"), (JsValue::Undefined, "undefined")] {
        for method in ["steer_turn", "cancel_steer"] {
            let error = requests
                .request(method.into(), params.clone())
                .await
                .expect_err("#1604 nullish params must throw on the sessionKey read");
            negative_assert_eq!(
                error.code,
                "gateway_request_failed",
                "#1604 nullish params use the boundary code ({method})"
            );
            negative_assert_eq!(
                error.message,
                pilotdeck_protocol::JsString::from(format!(
                    "Cannot read properties of {reading} (reading 'sessionKey')"
                )),
                "#1604 the property-read TypeError names the first field ({method})"
            );
        }
    }
    let steer_params = |session_key: JsValue, run_id: JsValue| {
        crate::metadata::object([
            ("sessionKey", session_key),
            ("runId", run_id),
            ("itemId", "i".into()),
            ("message", "m".into()),
        ])
    };
    // A non-string or blank session key is a Map miss exactly like an absent
    // one: the TS lookup never errors (probes s1-s3, c1/c2).
    for key in [JsValue::Number(5.0), " ".into(), JsValue::Bool(true)] {
        let result = requests
            .request("steer_turn".into(), steer_params(key.clone(), "r".into()))
            .await
            .expect("#1604 non-string session keys stay Ok on steer_turn");
        negative_assert_eq!(
            result.get("accepted"),
            Some(&JsValue::Bool(false)),
            "#1604 non-string session key is a no_active_turn miss"
        );
        negative_assert_eq!(
            result.get("reason").and_then(JsValue::as_string),
            Some(&pilotdeck_protocol::JsString::from("no_active_turn")),
            "#1604 non-string session key reports no_active_turn, not an error"
        );
        let result = requests
            .request("cancel_steer".into(), steer_params(key.clone(), "r".into()))
            .await
            .expect("#1604 non-string session keys stay Ok on cancel_steer");
        negative_assert_eq!(
            result.get("cancelled"),
            Some(&JsValue::Bool(false)),
            "#1604 non-string session key is a no_active_turn miss (cancel)"
        );
        negative_assert_eq!(
            result.get("reason").and_then(JsValue::as_string),
            Some(&pilotdeck_protocol::JsString::from("no_active_turn")),
            "#1604 non-string session key reports no_active_turn (cancel)"
        );
    }
    // Stage the state both gates require, then check the strict-equality
    // branches: a non-string runId mismatches instead of erroring (probes
    // s4, s10, c3).
    let _reservation = requests
        .owner
        .begin_turn(&"s".into(), &"r".into())
        .expect("#1604 admission slot");
    let steer = steer_resource(&requests);
    let _turn = start_steer_run(&steer, "s", "t", "r");
    for method in ["steer_turn", "cancel_steer"] {
        let result = requests
            .request(
                method.into(),
                steer_params("s".into(), JsValue::Number(5.0)),
            )
            .await
            .expect("#1604 non-string runId stays Ok ({method})");
        let field = if method == "steer_turn" {
            "accepted"
        } else {
            "cancelled"
        };
        negative_assert_eq!(
            result.get(field),
            Some(&JsValue::Bool(false)),
            "#1604 non-string runId takes the mismatch branch ({method})"
        );
        negative_assert_eq!(
            result.get("reason").and_then(JsValue::as_string),
            Some(&pilotdeck_protocol::JsString::from("turn_mismatch")),
            "#1604 non-string runId reports turn_mismatch ({method})"
        );
    }
    // A non-string session key stays a miss even with a live turn (probe s2).
    let result = requests
        .request(
            "steer_turn".into(),
            steer_params(JsValue::Number(5.0), "r".into()),
        )
        .await
        .expect("#1604 non-string session key with a live turn stays Ok");
    negative_assert_eq!(
        result.get("accepted"),
        Some(&JsValue::Bool(false)),
        "#1604 non-string session key misses the live turn too"
    );
    // Positive: with every field a matching string the steer is accepted and
    // the same item can be cancelled again (m3-style round trip).
    let result = requests
        .request("steer_turn".into(), steer_params("s".into(), "r".into()))
        .await
        .expect("#1604 matching steer stays Ok");
    negative_assert_eq!(
        result.get("accepted"),
        Some(&JsValue::Bool(true)),
        "#1604 matching steer is accepted"
    );
    let result = requests
        .request("cancel_steer".into(), steer_params("s".into(), "r".into()))
        .await
        .expect("#1604 matching cancel stays Ok");
    negative_assert_eq!(
        result.get("cancelled"),
        Some(&JsValue::Bool(true)),
        "#1604 the enqueued item is cancelled by its id"
    );
    requests
        .owner
        .shutdown()
        .await
        .expect("#1604 registry fixture owner shutdown");
    requests
        .project
        .dispose()
        .await
        .expect("#1604 registry fixture project shutdown");
}

#[tokio::test]
async fn steer_item_id_coerces_to_text_for_the_mailbox_identity() {
    // Ledger A7.2-STEER-INPUT-001: TS `steerTurn` embeds `input.itemId`
    // verbatim in the canonical message (probe s5/s8), but the Rust mailbox
    // keys an item by a `JsString`, so a non-string id travels as its
    // unchecked `String()` form. The response frame keeps parity on the
    // single-request cells (accepted, no self-invented error); the durable
    // record and the mailbox identity carry the coerced text — and because
    // the identity is the coerced text, cross-type same-text interleavings
    // alias in Rust while TS keys by the raw wire value (§交错 A/B, §缺席格
    // and the explicit-null cells below: absent → "undefined", null → "null").
    use crate::runtime::turn::gateway::GatewayRequests;

    let (_scratch, requests) = std::panic::catch_unwind(super::super::management_tests::setup)
        .expect("#1604 registry fixture setup");
    let _reservation = requests
        .owner
        .begin_turn(&"s".into(), &"r".into())
        .expect("#1604 admission slot");
    let steer = steer_resource(&requests);
    let _turn = start_steer_run(&steer, "s", "t", "r");
    let steer_params = |item_id: JsValue| {
        crate::metadata::object([
            ("sessionKey", "s".into()),
            ("runId", "r".into()),
            ("itemId", item_id),
            ("message", "m".into()),
        ])
    };
    // Response-frame parity: number, array and `{toString: null}` item ids
    // are all accepted (probe s5/s6/s8 — TS never errors on the item id).
    for item_id in [
        JsValue::Number(5.0),
        JsValue::Array(vec![JsValue::Number(1.0), JsValue::Number(2.0)]),
        crate::metadata::object([("toString", JsValue::Null)]),
    ] {
        let result = requests
            .request("steer_turn".into(), steer_params(item_id))
            .await
            .expect("#1604 non-string item ids stay Ok");
        negative_assert_eq!(
            result.get("accepted"),
            Some(&JsValue::Bool(true)),
            "#1604 non-string item id is accepted like TS"
        );
    }
    // The mailbox identity and `queueItemId` are the coerced text
    // (`String(5)`, `String([1,2])`, `String({toString:null})`).
    let drained = steer
        .mailbox_for_test(&"s".into())
        .expect("#1604 session mailbox")
        .drain(&"t".into());
    negative_assert_eq!(
        drained.len(),
        3,
        "#1604 all three non-string-id steers were enqueued"
    );
    for (message, coerced) in drained.iter().zip(["5", "1,2", "[object Object]"]) {
        negative_assert_eq!(
            message.item_id,
            pilotdeck_protocol::JsString::from(coerced),
            "#1604 the mailbox identity is the unchecked String() form"
        );
        let metadata = message
            .message
            .get("metadata")
            .expect("#1604 canonical metadata");
        negative_assert_eq!(
            metadata.get("queueItemId"),
            Some(&JsValue::String(pilotdeck_protocol::JsString::from(
                coerced
            ))),
            "#1604 queueItemId carries the coerced text (A7.2-STEER-INPUT-001)"
        );
    }
    // Registered divergence: a nesting depth whose `String()` throws in the
    // checked converter reports the RangeError instead of TS's verbatim
    // embed; the failure is the boundary's, not a self-invented guard.
    let mut deep = JsValue::Number(7.0);
    for _ in 0..=crate::js_value::MAX_ARRAY_DEPTH {
        deep = JsValue::Array(vec![deep]);
    }
    let error = requests
        .request("steer_turn".into(), steer_params(deep))
        .await
        .expect_err("#1604 a too-deep item id reports the join RangeError");
    negative_assert_eq!(
        error.code,
        "gateway_request_failed",
        "#1604 the deep-array item id uses the boundary code"
    );
    negative_assert_eq!(
        error.message,
        pilotdeck_protocol::JsString::from(crate::js_value::StackOverflow::MESSAGE),
        "#1604 the deep-array item id reports the V8 RangeError text"
    );
    // §交错 A (r1 concurrency P1-b, A7.2-STEER-INPUT-001): the numeric cancel
    // and the later string steer of the same text alias in Rust. TS keyed the
    // cancellation on the number `5` itself, so the string steer is accepted
    // and delivered (real-SteerMailbox probe
    // `_tmp/20261108-review-2382-ds/ts-probe/output.txt`, cells
    // A_cancel_number5 / A_enqueue_string5_after, Node v22.22.1); Rust's
    // text-keyed `cancelled` set rejects it — a response-frame divergence the
    // ledger registers.
    let _reservation_a = requests
        .owner
        .begin_turn(&"ia".into(), &"ra".into())
        .expect("#1604 interleave-A admission slot");
    let _turn_a = start_steer_run(&steer, "ia", "ta", "ra");
    let cancelled = requests
        .request(
            "cancel_steer".into(),
            crate::metadata::object([
                ("sessionKey", "ia".into()),
                ("runId", "ra".into()),
                ("itemId", JsValue::Number(5.0)),
            ]),
        )
        .await
        .expect("#1604 interleave-A numeric cancel stays Ok");
    negative_assert_eq!(
        cancelled.get("cancelled"),
        Some(&JsValue::Bool(true)),
        "#1604 interleave A: the numeric cancel records like TS"
    );
    let steered = requests
        .request(
            "steer_turn".into(),
            crate::metadata::object([
                ("sessionKey", "ia".into()),
                ("runId", "ra".into()),
                ("itemId", "5".into()),
                ("message", "late".into()),
            ]),
        )
        .await
        .expect("#1604 interleave-A string steer stays Ok");
    negative_assert_eq!(
        steered.get("accepted"),
        Some(&JsValue::Bool(false)),
        "#1604 interleave A: the string steer hits the numeric cancel (Rust divergence)"
    );
    negative_assert_eq!(
        steered.get("reason").and_then(JsValue::as_string),
        Some(&pilotdeck_protocol::JsString::from("cancelled")),
        "#1604 interleave A: the rejection reason is cancelled"
    );
    negative_assert_eq!(
        steer
            .mailbox_for_test(&"ia".into())
            .expect("#1604 interleave-A mailbox")
            .drain(&"ta".into())
            .len(),
        0,
        "#1604 interleave A: nothing is enqueued (TS delivers the string steer)"
    );
    // §交错 B: the dedupe swallows the second same-text message and the
    // cancel removes the first (numeric) entry. TS keeps both identities
    // apart (probe cells B_enqueue_number5 / B_enqueue_string5 /
    // B_cancel_string5 / B_pending_after_drain:[5]): the string cancel
    // removes only the string entry and the numeric one survives the drain.
    let _reservation_b = requests
        .owner
        .begin_turn(&"ib".into(), &"rb".into())
        .expect("#1604 interleave-B admission slot");
    let _turn_b = start_steer_run(&steer, "ib", "tb", "rb");
    let first = requests
        .request(
            "steer_turn".into(),
            crate::metadata::object([
                ("sessionKey", "ib".into()),
                ("runId", "rb".into()),
                ("itemId", JsValue::Number(9.0)),
                ("message", "first".into()),
            ]),
        )
        .await
        .expect("#1604 interleave-B numeric steer stays Ok");
    negative_assert_eq!(
        first.get("accepted"),
        Some(&JsValue::Bool(true)),
        "#1604 interleave B: the numeric steer is accepted"
    );
    let second = requests
        .request(
            "steer_turn".into(),
            crate::metadata::object([
                ("sessionKey", "ib".into()),
                ("runId", "rb".into()),
                ("itemId", "9".into()),
                ("message", "second".into()),
            ]),
        )
        .await
        .expect("#1604 interleave-B string steer stays Ok");
    negative_assert_eq!(
        second.get("accepted"),
        Some(&JsValue::Bool(true)),
        "#1604 interleave B: the string steer answers accepted while Rust dedupes it away (TS enqueues an independent second entry)"
    );
    let cancelled = requests
        .request(
            "cancel_steer".into(),
            crate::metadata::object([
                ("sessionKey", "ib".into()),
                ("runId", "rb".into()),
                ("itemId", "9".into()),
            ]),
        )
        .await
        .expect("#1604 interleave-B string cancel stays Ok");
    negative_assert_eq!(
        cancelled.get("cancelled"),
        Some(&JsValue::Bool(true)),
        "#1604 interleave B: the string cancel removes the numeric entry (Rust divergence)"
    );
    negative_assert_eq!(
        steer
            .mailbox_for_test(&"ib".into())
            .expect("#1604 interleave-B mailbox")
            .drain(&"tb".into())
            .len(),
        0,
        "#1604 interleave B: the numeric message is silently lost (TS drains [9])"
    );
    // §缺席格 (r1 boundary#1, folded into P1-b): an absent `itemId` coerces to
    // the literal text "undefined" and aliases a genuine string id of that
    // spelling. TS embeds the raw `undefined` (JSON.stringify drops
    // `metadata.queueItemId` entirely) and keys the mailbox by the undefined
    // value, so the literal-string entry survives its own cancel of the
    // absent identity; Rust cancels it.
    let _reservation_c = requests
        .owner
        .begin_turn(&"ic".into(), &"rc".into())
        .expect("#1604 absent-id admission slot");
    let _turn_c = start_steer_run(&steer, "ic", "tc", "rc");
    let absent = requests
        .request(
            "steer_turn".into(),
            crate::metadata::object([
                ("sessionKey", "ic".into()),
                ("runId", "rc".into()),
                ("message", "m".into()),
            ]),
        )
        .await
        .expect("#1604 the absent itemId steer stays Ok");
    negative_assert_eq!(
        absent.get("accepted"),
        Some(&JsValue::Bool(true)),
        "#1604 an absent itemId is accepted like TS"
    );
    let literal = requests
        .request(
            "steer_turn".into(),
            crate::metadata::object([
                ("sessionKey", "ic".into()),
                ("runId", "rc".into()),
                ("itemId", "undefined".into()),
                ("message", "literal".into()),
            ]),
        )
        .await
        .expect("#1604 the literal undefined steer stays Ok");
    negative_assert_eq!(
        literal.get("accepted"),
        Some(&JsValue::Bool(true)),
        "#1604 the literal string id answers accepted while Rust dedupes it against the absent id"
    );
    let cancelled = requests
        .request(
            "cancel_steer".into(),
            crate::metadata::object([("sessionKey", "ic".into()), ("runId", "rc".into())]),
        )
        .await
        .expect("#1604 the absent-id cancel stays Ok");
    negative_assert_eq!(
        cancelled.get("cancelled"),
        Some(&JsValue::Bool(true)),
        "#1604 the absent-id cancel removes the literal entry (Rust divergence)"
    );
    negative_assert_eq!(
        steer
            .mailbox_for_test(&"ic".into())
            .expect("#1604 absent-id mailbox")
            .drain(&"tc".into())
            .len(),
        0,
        "#1604 the literal-string message is silently lost (TS keeps it)"
    );
    // The coerced durable form of the absent id: the mailbox identity and
    // `queueItemId` carry the string "undefined" (TS's raw undefined makes
    // JSON.stringify drop the metadata key).
    let _reservation_d = requests
        .owner
        .begin_turn(&"id".into(), &"rd".into())
        .expect("#1604 absent-coercion admission slot");
    let _turn_d = start_steer_run(&steer, "id", "td", "rd");
    requests
        .request(
            "steer_turn".into(),
            crate::metadata::object([
                ("sessionKey", "id".into()),
                ("runId", "rd".into()),
                ("message", "m".into()),
            ]),
        )
        .await
        .expect("#1604 the absent-coercion steer stays Ok");
    let drained = steer
        .mailbox_for_test(&"id".into())
        .expect("#1604 absent-coercion mailbox")
        .drain(&"td".into());
    negative_assert_eq!(drained.len(), 1, "#1604 the absent-id steer was enqueued");
    negative_assert_eq!(
        drained[0].item_id,
        pilotdeck_protocol::JsString::from("undefined"),
        "#1604 the absent itemId coerces to the literal undefined text"
    );
    let metadata = drained[0]
        .message
        .get("metadata")
        .expect("#1604 absent-coercion canonical metadata");
    negative_assert_eq!(
        metadata.get("queueItemId"),
        Some(&JsValue::String(pilotdeck_protocol::JsString::from(
            "undefined"
        ))),
        "#1604 queueItemId carries the undefined text (A7.2-STEER-INPUT-001)"
    );
    // §null 强转格 (r2 boundary#1): an explicit `itemId: null` coerces to the
    // literal text "null" — a third alias class of the same boundary, not the
    // "undefined" of the absent key. TS embeds the raw `null` (probe
    // `20261109-review-2382-r2-ds/ts-probe/output5.txt` g2: `queueItemId:null`).
    let _reservation_e = requests
        .owner
        .begin_turn(&"ie".into(), &"re".into())
        .expect("#1604 null-id admission slot");
    let _turn_e = start_steer_run(&steer, "ie", "te", "re");
    let nulled = requests
        .request(
            "steer_turn".into(),
            crate::metadata::object([
                ("sessionKey", "ie".into()),
                ("runId", "re".into()),
                ("itemId", JsValue::Null),
                ("message", "m".into()),
            ]),
        )
        .await
        .expect("#1604 the null itemId steer stays Ok");
    negative_assert_eq!(
        nulled.get("accepted"),
        Some(&JsValue::Bool(true)),
        "#1604 a null itemId is accepted like TS"
    );
    let drained = steer
        .mailbox_for_test(&"ie".into())
        .expect("#1604 null-id mailbox")
        .drain(&"te".into());
    negative_assert_eq!(drained.len(), 1, "#1604 the null-id steer was enqueued");
    negative_assert_eq!(
        drained[0].item_id,
        pilotdeck_protocol::JsString::from("null"),
        "#1604 the null itemId coerces to the literal null text"
    );
    let metadata = drained[0]
        .message
        .get("metadata")
        .expect("#1604 null-id canonical metadata");
    negative_assert_eq!(
        metadata.get("queueItemId"),
        Some(&JsValue::String(pilotdeck_protocol::JsString::from("null"))),
        "#1604 queueItemId carries the null text while TS embeds the raw null (A7.2-STEER-INPUT-001)"
    );
    // §null 交错格: TS keys the mailbox by the raw `null` value, so the
    // literal `"null"` string steers independently and survives a cancel of
    // its own spelling (probe `output5.txt` null_pending_after_drain:[null]);
    // Rust's text identity merges the two and the cancel removes the entry.
    let _reservation_f = requests
        .owner
        .begin_turn(&"if".into(), &"rf".into())
        .expect("#1604 null-interleave admission slot");
    let _turn_f = start_steer_run(&steer, "if", "tf", "rf");
    let nulled = requests
        .request(
            "steer_turn".into(),
            crate::metadata::object([
                ("sessionKey", "if".into()),
                ("runId", "rf".into()),
                ("itemId", JsValue::Null),
                ("message", "m".into()),
            ]),
        )
        .await
        .expect("#1604 the null-interleave steer stays Ok");
    negative_assert_eq!(
        nulled.get("accepted"),
        Some(&JsValue::Bool(true)),
        "#1604 the null interleave steer is accepted like TS"
    );
    let literal = requests
        .request(
            "steer_turn".into(),
            crate::metadata::object([
                ("sessionKey", "if".into()),
                ("runId", "rf".into()),
                ("itemId", "null".into()),
                ("message", "literal".into()),
            ]),
        )
        .await
        .expect("#1604 the literal null steer stays Ok");
    negative_assert_eq!(
        literal.get("accepted"),
        Some(&JsValue::Bool(true)),
        "#1604 the literal null-string id answers accepted while Rust dedupes it against the null id"
    );
    let cancelled = requests
        .request(
            "cancel_steer".into(),
            crate::metadata::object([
                ("sessionKey", "if".into()),
                ("runId", "rf".into()),
                ("itemId", "null".into()),
            ]),
        )
        .await
        .expect("#1604 the literal null-string cancel stays Ok");
    negative_assert_eq!(
        cancelled.get("cancelled"),
        Some(&JsValue::Bool(true)),
        "#1604 the literal cancel removes the null-aliased entry (Rust divergence)"
    );
    negative_assert_eq!(
        steer
            .mailbox_for_test(&"if".into())
            .expect("#1604 null-interleave mailbox")
            .drain(&"tf".into())
            .len(),
        0,
        "#1604 the null-id message is silently lost (TS drains [null])"
    );
    // §差异 6 claim 门（r3 boundary#1，A7.2-STEER-INPUT-001）：durable 消费
    // 时 Rust 的 claim 门读 `metadata.queueItemId` 并以非空文本放行——
    // 真值行为假的非字符串 id（缺席/null/0/false）在入参边界已折成非空
    // 文本（`"undefined"`/`"null"`/`"0"`/`"false"`），一律走 claim，注入的
    // recorder 落 `{"mutation":"claim","itemId":<强转文本>}`；空数组 `[]` 折成
    // 空文本 `""`，被非空文本门跳过。TS `TurnRunner.onDurableMessage` 以
    // 真值门过滤原始 `queueItemId`（TurnRunner.ts `if (itemId)`）：缺席/
    // null/0/false/""/缺键跳过 claim，而 `[]` 在 JS 为真值、走 claim
    // （probe `20261109-review-2382-r5-ds/ts-probe/output6.txt`，Node
    // v22.22.0——`""` 双方都跳过，对齐）。经 `start_turn` 装进 router 的
    // drain 端口（TurnSteer 路径，填充 unacknowledged）后再
    // `persist_and_apply`，钉住两侧方向相反的 claim 分歧。
    for (session, run, turn_id, item_id, mailbox_id, claim, close_discard) in [
        (
            "ig",
            "igr",
            "igt",
            None::<JsValue>,
            "undefined",
            Some("undefined"),
            false,
        ),
        (
            "ih",
            "ihr",
            "iht",
            Some(JsValue::Null),
            "null",
            Some("null"),
            false,
        ),
        (
            "ii",
            "iir",
            "iit",
            Some(JsValue::Number(0.0)),
            "0",
            Some("0"),
            false,
        ),
        (
            "ij",
            "ijr",
            "ijt",
            Some(JsValue::Bool(false)),
            "false",
            Some("false"),
            false,
        ),
        (
            "ik",
            "ikr",
            "ikt",
            Some(JsValue::Array(vec![])),
            "",
            None,
            true,
        ),
        (
            "il",
            "ilr",
            "ilt",
            Some(JsValue::String(pilotdeck_protocol::JsString::from(""))),
            "",
            None,
            false,
        ),
    ] {
        let _reservation = requests
            .owner
            .begin_turn(&session.into(), &run.into())
            .expect("#1604 claim-gate admission slot");
        let recorded = Arc::new(RecordedClaims::default());
        let recorder: Arc<dyn crate::runtime::steer::MutationRecorder> = recorded.clone();
        let mut router = crate::host::modules::ModuleRouter::default();
        let turn = steer
            .bind_session(session.into(), Some(recorder))
            .expect("#1604 claim-gate session binding")
            .start_turn(
                turn_id.into(),
                run.into(),
                vec![],
                crate::runtime::hooks::Cancellation::new(),
                &mut router,
            )
            .expect("#1604 claim-gate turn start");
        let mut params = vec![
            ("sessionKey", JsValue::from(session)),
            ("runId", JsValue::from(run)),
        ];
        if let Some(item_id) = item_id {
            params.push(("itemId", item_id));
        }
        params.push(("message", "m".into()));
        requests
            .request(
                "steer_turn".into(),
                crate::metadata::object(params.into_iter()),
            )
            .await
            .expect("#1604 the claim-gate steer stays Ok");
        let drain_port = router
            .turn
            .ports
            .drain_steer
            .take()
            .expect("#1604 claim-gate drain port");
        let drained = drain_port()
            .await
            .expect("#1604 the claim-gate turn-steer drain");
        negative_assert_eq!(
            drained.len(),
            1,
            "#1604 the claim-gate drain offered the steer"
        );
        negative_assert_eq!(
            drained[0].get("itemId"),
            Some(&JsValue::String(pilotdeck_protocol::JsString::from(
                mailbox_id
            ))),
            "#1604 the claim-gate mailbox identity is the coerced text"
        );
        // The durable record is written whether or not the claim gate passes
        // (probe6 `durable_recorded_count: 10` — all ten TS cells record);
        // counting it pins that the skipped-claim cells still persist.
        let durable_records = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let durable_counter = durable_records.clone();
        turn.persist_and_apply(
            pilotdeck_protocol::JsString::from(mailbox_id),
            drained[0]
                .get("message")
                .expect("#1604 claim-gate canonical message")
                .clone(),
            move |_| {
                durable_counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                Box::pin(async { Ok::<(), crate::host::HostError>(()) })
            },
        )
        .await
        .expect("#1604 the durable apply of the coerced id succeeds");
        negative_assert_eq!(
            durable_records.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "#1604 the durable record is written even when the claim gate skips (probe6 durable_recorded_count: 10)"
        );
        let claims = recorded.claims.lock().unwrap().clone();
        if let Some(claim) = claim {
            negative_assert_eq!(
                claims.len(),
                1,
                "#1604 the coerced id claims the mailbox entry while TS's truthiness gate records no claim (A7.2-STEER-INPUT-001 差异 6)"
            );
            negative_assert_eq!(
                claims[0],
                pilotdeck_protocol::JsString::from(claim),
                "#1604 the claim mutation keys by the coerced text"
            );
        } else {
            negative_assert!(
                claims.is_empty(),
                "#1604 the empty coerced text skips the claim gate like the TS empty string (A7.2-STEER-INPUT-001 差异 6)"
            );
        }
        // The skipped-claim `[]` entry stays Offered — the applied-path ack
        // only removes Claimed entries — so closing the turn records a
        // `turn_ended` discard for it, while TS's `[]` is claimed (probe6)
        // and acked on apply, leaving nothing to discard (probe8 A;
        // A7.2-STEER-INPUT-001 差异 6 下游).
        if close_discard {
            let discarded = steer
                .mailbox_for_test(&session.into())
                .expect("#1604 claim-gate mailbox")
                .close(pilotdeck_protocol::JsString::from(turn_id))
                .await
                .expect("#1604 the turn-end close of the skipped-claim entry");
            negative_assert_eq!(
                discarded.len(),
                1,
                "#1604 the skipped-claim entry is discarded at turn end while TS's claimed-and-acked empty array is not (A7.2-STEER-INPUT-001 差异 6)"
            );
            let discards = recorded.discards.lock().unwrap().clone();
            negative_assert_eq!(
                discards.len(),
                1,
                "#1604 the turn-end close records exactly one discard mutation"
            );
            negative_assert_eq!(
                discards[0].0,
                pilotdeck_protocol::JsString::from(""),
                "#1604 the discard mutation keys by the coerced empty text"
            );
            negative_assert_eq!(
                discards[0].1,
                Some(pilotdeck_protocol::JsString::from("turn_ended")),
                "#1604 the discard mutation carries the turn_ended reason"
            );
        }
    }
    // §差异 7 空文本交错格：`[]` 与 `""` 都折成空文本 `""`，互为别名——
    // 第二条被 seen 去重静默丢弃，cancel `""` 移除 `[]` 条。TS 的 `[]` 值
    // 与 `""` 值不碰撞：两条都入队，cancel `""` 只取消字符串条，drain 回
    // `[[]]`（probe `20261109-review-2382-r5-ds/ts-probe/output7.txt`，
    // Node v22.22.0）。
    let _reservation_m = requests
        .owner
        .begin_turn(&"im".into(), &"imr".into())
        .expect("#1604 empty-text interleave admission slot");
    let _turn_m = start_steer_run(&steer, "im", "tmt", "imr");
    let empty_array = requests
        .request(
            "steer_turn".into(),
            crate::metadata::object([
                ("sessionKey", "im".into()),
                ("runId", "imr".into()),
                ("itemId", JsValue::Array(vec![])),
                ("message", "m".into()),
            ]),
        )
        .await
        .expect("#1604 the empty-array steer stays Ok");
    negative_assert_eq!(
        empty_array.get("accepted"),
        Some(&JsValue::Bool(true)),
        "#1604 the empty-array steer is accepted like TS"
    );
    let empty_string = requests
        .request(
            "steer_turn".into(),
            crate::metadata::object([
                ("sessionKey", "im".into()),
                ("runId", "imr".into()),
                ("itemId", "".into()),
                ("message", "second".into()),
            ]),
        )
        .await
        .expect("#1604 the empty-string steer stays Ok");
    negative_assert_eq!(
        empty_string.get("accepted"),
        Some(&JsValue::Bool(true)),
        "#1604 the empty-string steer answers accepted while Rust dedupes it against the empty-array id"
    );
    let cancelled = requests
        .request(
            "cancel_steer".into(),
            crate::metadata::object([
                ("sessionKey", "im".into()),
                ("runId", "imr".into()),
                ("itemId", "".into()),
            ]),
        )
        .await
        .expect("#1604 the empty-text cancel stays Ok");
    negative_assert_eq!(
        cancelled.get("cancelled"),
        Some(&JsValue::Bool(true)),
        "#1604 the empty-text cancel removes the empty-array entry (Rust divergence)"
    );
    negative_assert_eq!(
        steer
            .mailbox_for_test(&"im".into())
            .expect("#1604 empty-text mailbox")
            .drain(&"tmt".into())
            .len(),
        0,
        "#1604 the empty-array message is silently lost (TS drains [[]])"
    );
    // §差异 7 反向格：先 steer `[]` 再 cancel `[]`——Rust 的 cancel 折成
    // `""`、命中同键条目（cancelled:true、drain 0）；TS 的 cancel 收到的是
    // 新数组引用，SameValueZero 永不匹配（cancelled:true 但条目存活，
    // drain 回 [[]]，probe output7 `empty_cancel_array*`）。
    let _reservation_n = requests
        .owner
        .begin_turn(&"in".into(), &"inr".into())
        .expect("#1604 reverse empty-text admission slot");
    let _turn_n = start_steer_run(&steer, "in", "tnt", "inr");
    let reverse_steer = requests
        .request(
            "steer_turn".into(),
            crate::metadata::object([
                ("sessionKey", "in".into()),
                ("runId", "inr".into()),
                ("itemId", JsValue::Array(vec![])),
                ("message", "m".into()),
            ]),
        )
        .await
        .expect("#1604 the reverse empty-text steer stays Ok");
    negative_assert_eq!(
        reverse_steer.get("accepted"),
        Some(&JsValue::Bool(true)),
        "#1604 the reverse empty-text steer is accepted like TS"
    );
    let reverse_cancel = requests
        .request(
            "cancel_steer".into(),
            crate::metadata::object([
                ("sessionKey", "in".into()),
                ("runId", "inr".into()),
                ("itemId", JsValue::Array(vec![])),
            ]),
        )
        .await
        .expect("#1604 the reverse empty-text cancel stays Ok");
    negative_assert_eq!(
        reverse_cancel.get("cancelled"),
        Some(&JsValue::Bool(true)),
        "#1604 the empty-array cancel answers cancelled like TS"
    );
    negative_assert_eq!(
        steer
            .mailbox_for_test(&"in".into())
            .expect("#1604 reverse empty-text mailbox")
            .drain(&"tnt".into())
            .len(),
        0,
        "#1604 the empty-array cancel removes the empty-array entry (TS's new reference never matches, drain [[]])"
    );
    requests
        .owner
        .shutdown()
        .await
        .expect("#1604 registry fixture owner shutdown");
    requests
        .project
        .dispose()
        .await
        .expect("#1604 registry fixture project shutdown");
}

#[tokio::test]
async fn hook_async_result_non_string_ids_take_the_optional_chain_type_error() {
    // `require_text` three-layer shape (probes h4/h5/h10/h11): a non-string id
    // is the boundary's `input.<key>?.trim is not a function` TypeError, and
    // Unicode blank still folds to the TS domain error (h7/h8).
    use crate::runtime::turn::gateway::GatewayRequests;

    let (_scratch, requests) = std::panic::catch_unwind(super::super::management_tests::setup)
        .expect("#1604 registry fixture setup");
    for (key, field) in [
        (JsValue::Number(5.0), "sessionKey"),
        (JsValue::Bool(true), "sessionKey"),
        (
            crate::metadata::object([("toString", JsValue::Null)]),
            "sessionKey",
        ),
        (JsValue::Number(5.0), "invocationId"),
    ] {
        let other = if field == "sessionKey" {
            "invocationId"
        } else {
            "sessionKey"
        };
        let params = crate::metadata::object([(field, key.clone()), (other, "i".into())]);
        let error = requests
            .request("hook_async_result".into(), params)
            .await
            .expect_err("#1604 non-string ids must take the TypeError path");
        negative_assert_eq!(
            error.code,
            "gateway_request_failed",
            "#1604 non-string {field} uses the boundary code"
        );
        negative_assert_eq!(
            error.message,
            pilotdeck_protocol::JsString::from(format!("input.{field}?.trim is not a function")),
            "#1604 non-string {field} echoes the optional-chain TypeError"
        );
    }
    // Nullish params (h1/h2): the read itself throws, sessionKey first.
    for (params, reading) in [(JsValue::Null, "null"), (JsValue::Undefined, "undefined")] {
        let error = requests
            .request("hook_async_result".into(), params)
            .await
            .expect_err("#1604 nullish params must throw on the sessionKey read");
        negative_assert_eq!(
            error.message,
            pilotdeck_protocol::JsString::from(format!(
                "Cannot read properties of {reading} (reading 'sessionKey')"
            )),
            "#1604 hook_async_result nullish params name the first field"
        );
    }
    // Unicode blank (ideographic space, BOM) is the domain error (h7/h8) —
    // the js_trim WhiteSpace set, wider than Rust `str::trim`.
    for params in [
        crate::metadata::object([("sessionKey", "　".into()), ("invocationId", "i".into())]),
        crate::metadata::object([("sessionKey", "s".into()), ("invocationId", "﻿".into())]),
    ] {
        let error = requests
            .request("hook_async_result".into(), params)
            .await
            .expect_err("#1604 Unicode blank ids are blank");
        negative_assert_eq!(
            error.code,
            "INVALID_ASYNC_HOOK_RESULT",
            "#1604 Unicode blank uses the TS domain code"
        );
        negative_assert_eq!(
            error.message,
            pilotdeck_protocol::JsString::from("sessionKey and invocationId are required."),
            "#1604 Unicode blank keeps the TS message"
        );
    }
    requests
        .owner
        .shutdown()
        .await
        .expect("#1604 registry fixture owner shutdown");
    requests
        .project
        .dispose()
        .await
        .expect("#1604 registry fixture project shutdown");
}

#[tokio::test]
async fn run_family_reads_the_registry_key_like_the_ts_template() {
    // The registry key template `` `${projectKey ?? ""}\0${sessionKey}
    // \0${runId}` `` (RunRegistry.ts:70) coerces every field; TS validates
    // only afterSeq/limit, and only after the capability gate. Seeded like
    // probe2 (`output2.txt`): a record under the literal "undefined" strings
    // plus one normal run.
    use crate::runtime::turn::gateway::GatewayRequests;
    use crate::runtime::turn::publish::TurnRunRegistry;
    use crate::storage::{RunAcceptInput, RunRef};

    let (_scratch, requests) = std::panic::catch_unwind(super::super::management_tests::setup)
        .expect("#1604 registry fixture setup");
    let registry = requests.runs.clone().expect("#1604 fixture run registry");
    for run in [
        RunRef {
            project_key: None,
            session_key: "undefined".into(),
            run_id: "undefined".into(),
        },
        RunRef {
            project_key: None,
            session_key: "s".into(),
            run_id: "r".into(),
        },
    ] {
        TurnRunRegistry::accept(
            &*registry,
            &RunAcceptInput {
                run,
                request_material: "#1604 seed".into(),
            },
        )
        .await
        .expect("#1604 seed accept");
    }
    // q1: runGet({}) reads `\0undefined\0undefined` and hits the record —
    // a missing field's template form is the literal text "undefined".
    let result = requests
        .request("run_get".into(), crate::metadata::object([]))
        .await
        .expect("#1604 run_get with all fields missing stays Ok");
    negative_assert_eq!(
        result.get("sessionKey").and_then(JsValue::as_string),
        Some(&pilotdeck_protocol::JsString::from("undefined")),
        "#1604 missing sessionKey/runId read as the literal undefined text"
    );
    // q6: runEvents({}) reads the same record.
    let result = requests
        .request("run_events".into(), crate::metadata::object([]))
        .await
        .expect("#1604 run_events with all fields missing stays Ok");
    negative_assert_eq!(
        result.get("events"),
        Some(&JsValue::Array(vec![])),
        "#1604 run_events on the literal-undefined record returns no events"
    );
    negative_assert_eq!(
        result.get("nextSeq"),
        None,
        "#1604 an empty page carries no nextSeq"
    );
    // r5: a miss reports the coerced runId text.
    let error = requests
        .request(
            "run_events".into(),
            crate::metadata::object([("sessionKey", "s".into()), ("runId", JsValue::Number(5.0))]),
        )
        .await
        .expect_err("#1604 an unregistered run reports RUN_NOT_FOUND");
    negative_assert_eq!(error.code, "RUN_NOT_FOUND", "#1604 run miss code");
    negative_assert_eq!(
        error.message,
        pilotdeck_protocol::JsString::from("Run 5 is not registered."),
        "#1604 the run id reports its coerced text"
    );
    // q3/q7: a `{toString: null}` field throws inside the template
    // conversion — the boundary serializes the TypeError.
    for method in ["run_get", "run_events"] {
        let error = requests
            .request(
                method.into(),
                crate::metadata::object([
                    ("sessionKey", "s".into()),
                    (
                        "runId",
                        crate::metadata::object([("toString", JsValue::Null)]),
                    ),
                ]),
            )
            .await
            .expect_err("#1604 a non-primitive runId must throw ({method})");
        negative_assert_eq!(
            error.code,
            "gateway_request_failed",
            "#1604 ToPrimitive failure uses the boundary code ({method})"
        );
        negative_assert_eq!(
            error.message,
            pilotdeck_protocol::JsString::from("Cannot convert object to primitive value"),
            "#1604 ToPrimitive failure keeps the V8 text ({method})"
        );
    }
    // q8: arrays join into the key instead of throwing.
    let result = requests
        .request(
            "run_get".into(),
            crate::metadata::object([
                (
                    "sessionKey",
                    JsValue::Array(vec![JsValue::Number(1.0), JsValue::Number(2.0)]),
                ),
                ("runId", "r".into()),
            ]),
        )
        .await
        .expect("#1604 array sessionKey joins, no error");
    negative_assert_eq!(
        matches!(result, JsValue::Null),
        true,
        "#1604 the joined key misses and run_get answers null"
    );
    // r7: cursor validation precedes the key template reads.
    let error = requests
        .request(
            "run_events".into(),
            crate::metadata::object([
                ("afterSeq", JsValue::Number(-1.0)),
                (
                    "sessionKey",
                    crate::metadata::object([("toString", JsValue::Null)]),
                ),
                ("runId", "r".into()),
            ]),
        )
        .await
        .expect_err("#1604 an invalid cursor wins over the coercion");
    negative_assert_eq!(
        error.code,
        "INVALID_RUN_CURSOR",
        "#1604 cursor validation precedes key coercion"
    );
    // r10/q4: nullish params throws at each method's own first field read.
    let error = requests
        .request("run_events".into(), JsValue::Null)
        .await
        .expect_err("#1604 run_events null params must throw on afterSeq");
    negative_assert_eq!(
        error.message,
        pilotdeck_protocol::JsString::from("Cannot read properties of null (reading 'afterSeq')"),
        "#1604 run_events reads afterSeq first"
    );
    let error = requests
        .request("run_get".into(), JsValue::Null)
        .await
        .expect_err("#1604 run_get null params must throw on projectKey");
    negative_assert_eq!(
        error.message,
        pilotdeck_protocol::JsString::from("Cannot read properties of null (reading 'projectKey')"),
        "#1604 run_get reads projectKey first"
    );
    // r11/r12: the capability gate precedes every input read. r13-r15
    // (probe4.mts, r1 boundary#2): `runEvents` gates the same way — TS
    // answers CAPABILITY_UNAVAILABLE for nullish params, an empty object,
    // and even an invalid cursor when the registry is unconfigured, so the
    // gate wins over the afterSeq read and the cursor validation alike.
    let mut no_registry = requests.clone();
    no_registry.runs = None;
    let error = no_registry
        .request("run_get".into(), JsValue::Null)
        .await
        .expect_err("#1604 no registry must gate before any read");
    negative_assert_eq!(
        error.code,
        "CAPABILITY_UNAVAILABLE",
        "#1604 the capability gate precedes the field reads"
    );
    for (params, cell) in [
        (JsValue::Null, "r13 nullish"),
        (crate::metadata::object([]), "r14 empty object"),
        (
            crate::metadata::object([("afterSeq", JsValue::Number(-1.0))]),
            "r15 invalid cursor",
        ),
    ] {
        let error = no_registry
            .request("run_events".into(), params)
            .await
            .expect_err("#1604 run_events must gate before any read ({cell})");
        negative_assert_eq!(
            error.code,
            "CAPABILITY_UNAVAILABLE",
            "#1604 the run_events capability gate precedes reads and cursor validation ({cell})"
        );
    }
    requests
        .owner
        .shutdown()
        .await
        .expect("#1604 registry fixture owner shutdown");
    requests
        .project
        .dispose()
        .await
        .expect("#1604 registry fixture project shutdown");
}

#[tokio::test]
async fn active_turn_snapshot_misses_echo_the_raw_session_key() {
    // getActiveTurnSnapshot → GatewayTurnReplayStore.snapshot
    // (GatewayTurnReplayStore.ts:107-118): a non-string key is a Map miss
    // answered with the input's own value; the sessionKey entry is dropped
    // exactly when the field is absent (JSON.stringify drops undefined).
    use crate::runtime::turn::gateway::GatewayRequests;

    let (_scratch, requests) = std::panic::catch_unwind(super::super::management_tests::setup)
        .expect("#1604 registry fixture setup");
    // a4: nullish params throws on the sessionKey read.
    for (params, reading) in [(JsValue::Null, "null"), (JsValue::Undefined, "undefined")] {
        let error = requests
            .request("active_turn_snapshot".into(), params)
            .await
            .expect_err("#1604 nullish params must throw on the sessionKey read");
        negative_assert_eq!(
            error.message,
            pilotdeck_protocol::JsString::from(format!(
                "Cannot read properties of {reading} (reading 'sessionKey')"
            )),
            "#1604 active_turn_snapshot names the first field"
        );
    }
    // a1: an absent key drops the sessionKey entry from the miss object.
    let result = requests
        .request("active_turn_snapshot".into(), crate::metadata::object([]))
        .await
        .expect("#1604 the miss path stays Ok");
    negative_assert_eq!(
        result.get("active"),
        Some(&JsValue::Bool(false)),
        "#1604 the miss object reports inactive"
    );
    negative_assert_eq!(
        result.get("sessionKey"),
        None,
        "#1604 an absent sessionKey is dropped like JSON.stringify drops undefined"
    );
    negative_assert_eq!(
        result.get("events"),
        Some(&JsValue::Array(vec![])),
        "#1604 the miss object carries empty events"
    );
    // a2/a3/a5: any present key is echoed raw — number, blank string, null.
    for key in [JsValue::Number(5.0), " ".into(), JsValue::Null] {
        let result = requests
            .request(
                "active_turn_snapshot".into(),
                crate::metadata::object([("sessionKey", key.clone())]),
            )
            .await
            .expect("#1604 a raw sessionKey echo stays Ok");
        negative_assert_eq!(
            result.get("sessionKey"),
            Some(&key),
            "#1604 the miss object echoes the raw sessionKey"
        );
    }
    // a6: positive — a live replay answers through the coordinator.
    let _replay = requests.coordinator.start(&"s".into(), &"r".into(), |_| {});
    let result = requests
        .request(
            "active_turn_snapshot".into(),
            crate::metadata::object([("sessionKey", "s".into())]),
        )
        .await
        .expect("#1604 the live path stays Ok");
    negative_assert_eq!(
        result.get("active"),
        Some(&JsValue::Bool(true)),
        "#1604 a live replay reports active"
    );
    negative_assert_eq!(
        result.get("sessionKey").and_then(JsValue::as_string),
        Some(&pilotdeck_protocol::JsString::from("s")),
        "#1604 the live path echoes the string key"
    );
    negative_assert_eq!(
        result.get("runId").and_then(JsValue::as_string),
        Some(&pilotdeck_protocol::JsString::from("r")),
        "#1604 the live path reports the run id"
    );
    requests
        .owner
        .shutdown()
        .await
        .expect("#1604 registry fixture owner shutdown");
    requests
        .project
        .dispose()
        .await
        .expect("#1604 registry fixture project shutdown");
}
