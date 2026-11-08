use super::{
    AsyncHookRegistry, AsyncHookTurn, JsValue, ProjectResources, ProjectRuntimeRegistry, Resource,
    SessionOwner, TokioHookClock,
};
use crate::runtime::turn::coordinator::TurnEventCoordinator;
use std::sync::Arc;

pub(super) fn build_async_hook_registry(
    build_project: &Arc<ProjectRuntimeRegistry>,
    owner: &SessionOwner,
    coordinator: &Arc<TurnEventCoordinator>,
) -> Option<Arc<AsyncHookRegistry>> {
    // Build the async-hook registry only when the project stages an
    // InteractionSteer resource (i.e. the steer pathway is wired).
    // Check via ProjectResources→StageContext since InteractionSteer does
    // not implement RuntimeResources directly.
    let async_hook_registry = build_project
        .acquire(None)
        .ok()
        .and_then(|lease| {
            lease
                .runtime()
                .resources()
                .get::<ProjectResources>()?
                .context()
                .resource(Resource::InteractionSteer)
                .map(|_| ())
        })
        .map(|_| {
            let reg_project = build_project.clone();
            let reg_owner = owner.weak_active_turn_query();
            let reg_coordinator = coordinator.clone();
            let registry = AsyncHookRegistry::new(
                Arc::new(TokioHookClock),
                move |session_key| {
                    // Turn is live only when the admission slot holds a run ID.
                    let (project_key, run_id) = reg_owner(session_key)?;
                    // Shared lookup: fetch the InteractionSteer resource
                    // and guard the run ID against the admission slot.
                    let root = match project_key {
                        Some(key) => Some(std::path::PathBuf::from(key.to_utf8().ok()?)),
                        None => None,
                    };
                    let info = crate::runtime::project::interaction_steer::active_turn_for_project(
                        &reg_project,
                        root.as_deref(),
                        session_key,
                        Some(&run_id),
                    )?;
                    // Keep the full admission-locked handle: the registry must
                    // fence delivery by run identity, not by the textual turn
                    // ID a restarted run may reuse.
                    Some(AsyncHookTurn::new(info))
                },
                move |session_key, event| {
                    reg_coordinator.emit(session_key, &JsValue::Object(event));
                },
            );
            Arc::new(registry)
        });
    let _ = &async_hook_registry;
    async_hook_registry
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        host::modules::ModuleRouter,
        runtime::{
            async_hooks::{AsyncHookRegistration, AsyncHookStatus},
            hooks::Cancellation,
            project::{
                ProjectResources, ProjectRuntimeRegistry, Resource,
                interaction_steer::InteractionSteer,
            },
        },
    };
    use pilotdeck_protocol::{JsObject, JsString, JsValue};

    fn steer_resource(project: &Arc<ProjectRuntimeRegistry>) -> InteractionSteer {
        let lease = project.acquire(None).expect("#1450 project lease");
        lease
            .runtime()
            .resources()
            .get::<ProjectResources>()
            .expect("#1450 project resources")
            .context()
            .resource(Resource::InteractionSteer)
            .expect("#1450 interaction steer resource")
            .as_any()
            .downcast_ref::<InteractionSteer>()
            .expect("#1450 steer resource type")
            .clone()
    }

    /// Gate finding (P1-跟进 #1425 / #1307): the registry used to keep only
    /// the textual turn ID and a bare `SteerMailbox`, so a pending async-hook
    /// result from run r1 was delivered into a restarted run r2 that reused
    /// the same turn ID. Delivery must be fenced by run identity and go
    /// through the admission-locked `ActiveSteerInfo::enqueue`.
    #[tokio::test]
    async fn async_hook_result_cannot_cross_runs_after_a_same_text_turn_restart() {
        let (_scratch, requests) =
            std::panic::catch_unwind(crate::native_process::management_tests::setup)
                .expect("#1450 registry fixture setup");
        let session = JsString::from("hook-race");
        let reservation = requests
            .owner
            .begin_turn(&session, &"r1".into())
            .expect("#1450 admission slot");
        let steer = steer_resource(&requests.project);
        let binding = steer
            .bind_session(session.clone(), None)
            .expect("#1450 steer session binding");
        let first = binding
            .start_turn(
                "t".into(),
                "r1".into(),
                vec![],
                Cancellation::new(),
                &mut ModuleRouter::default(),
            )
            .expect("#1450 first steer turn start");
        let registry =
            build_async_hook_registry(&requests.project, &requests.owner, &requests.coordinator)
                .expect("#1450 async hook registry");
        registry
            .register(AsyncHookRegistration {
                session_key: session.clone(),
                invocation_id: "inv".into(),
                hook_name: "hook".into(),
                hook_event: "PreToolUse".into(),
                timeout_ms: Some(60_000.0),
                include_hook_events: true,
            })
            .expect("#1450 async hook registration");
        // r1 ends; the retry restarts a turn under the same textual turn ID.
        first.close().await.expect("#1450 first turn closed");
        requests
            .owner
            .end_turn(&session, Some(&"r1".into()), Some(&reservation));
        let _restarted_slot = requests
            .owner
            .begin_turn(&session, &"r2".into())
            .expect("#1450 restarted admission slot");
        let _second = binding
            .start_turn(
                "t".into(),
                "r2".into(),
                vec![],
                Cancellation::new(),
                &mut ModuleRouter::default(),
            )
            .expect("#1450 restarted steer turn start");
        let output = crate::metadata::object([(
            "hookSpecificOutput",
            crate::metadata::object([
                ("hookEventName", "PreToolUse".into()),
                ("additionalContext", "old-run context".into()),
            ]),
        )]);
        let result = registry
            .submit(session.clone(), "inv".into(), output)
            .await
            .expect("#1450 stale async hook result settles");
        assert_eq!(
            result.status,
            AsyncHookStatus::Expired,
            "#1450 an old run's async hook result must expire, not deliver"
        );
        let drained = steer
            .mailbox_for_test(&session)
            .expect("#1450 session mailbox")
            .drain(&"t".into());
        assert!(
            drained.is_empty(),
            "#1450 the restarted run received nothing"
        );
        // Positive control: r2's own hook still delivers.
        registry
            .register(AsyncHookRegistration {
                session_key: session.clone(),
                invocation_id: "fresh".into(),
                hook_name: "hook".into(),
                hook_event: "PreToolUse".into(),
                timeout_ms: Some(60_000.0),
                include_hook_events: false,
            })
            .expect("#1450 fresh registration");
        let fresh = registry
            .submit(
                session.clone(),
                "fresh".into(),
                JsValue::Object(JsObject::new()),
            )
            .await
            .expect("#1450 fresh result settles");
        assert_eq!(
            fresh.status,
            AsyncHookStatus::Delivered,
            "#1450 the restarted run's own hook still delivers"
        );
        requests
            .owner
            .shutdown()
            .await
            .expect("#1450 registry fixture owner shutdown");
        requests
            .project
            .dispose()
            .await
            .expect("#1450 registry fixture project shutdown");
    }

    /// P2-a (review 2026-11-06): positive control for the admission-locked
    /// delivery path. The cross-run test above only proves the fence rejects a
    /// restarted run; its "fresh" branch submits an empty context, which
    /// settles without entering the mailbox. Without a success case that runs
    /// `ActiveSteerInfo::enqueue` with a non-empty context, an `enqueue` that
    /// always rejected would still leave that test green. Assert the result is
    /// `Delivered` *and* that the steer message landed in the live run's
    /// mailbox carrying the hook's context.
    #[tokio::test]
    async fn async_hook_result_delivers_non_empty_context_through_real_enqueue() {
        let (_scratch, requests) =
            std::panic::catch_unwind(crate::native_process::management_tests::setup)
                .expect("#1450 registry fixture setup");
        let session = JsString::from("hook-delivery");
        let _slot = requests
            .owner
            .begin_turn(&session, &"r1".into())
            .expect("#1450 admission slot");
        let steer = steer_resource(&requests.project);
        let binding = steer
            .bind_session(session.clone(), None)
            .expect("#1450 steer session binding");
        let _turn = binding
            .start_turn(
                "t".into(),
                "r1".into(),
                vec![],
                Cancellation::new(),
                &mut ModuleRouter::default(),
            )
            .expect("#1450 steer turn start");
        let registry =
            build_async_hook_registry(&requests.project, &requests.owner, &requests.coordinator)
                .expect("#1450 async hook registry");
        registry
            .register(AsyncHookRegistration {
                session_key: session.clone(),
                invocation_id: "inv".into(),
                hook_name: "hook".into(),
                hook_event: "PreToolUse".into(),
                timeout_ms: Some(60_000.0),
                include_hook_events: false,
            })
            .expect("#1450 async hook registration");
        let output = crate::metadata::object([(
            "hookSpecificOutput",
            crate::metadata::object([
                ("hookEventName", "PreToolUse".into()),
                ("additionalContext", "live context".into()),
            ]),
        )]);
        let result = registry
            .submit(session.clone(), "inv".into(), output)
            .await
            .expect("#1450 live async hook result settles");
        assert_eq!(
            result.status,
            AsyncHookStatus::Delivered,
            "#1450 a live run's non-empty context must deliver"
        );
        let drained = steer
            .mailbox_for_test(&session)
            .expect("#1450 session mailbox")
            .drain(&"t".into());
        assert_eq!(
            drained.len(),
            1,
            "#1450 delivery must enter the live run's mailbox through real enqueue"
        );
        assert_eq!(drained[0].item_id, JsString::from("async-hook:inv"));
        let text = drained[0]
            .message
            .get("content")
            .and_then(|content| match content {
                JsValue::Array(items) => items.first(),
                _ => None,
            })
            .and_then(|item| item.get("text"))
            .and_then(JsValue::as_string)
            .expect("#1450 delivered steer carries the hook context");
        assert_eq!(
            text,
            &JsString::from(
                "<async_hook_context event=\"PreToolUse\">\nlive context\n</async_hook_context>"
            ),
            "#1450 delivered steer carries the hook context"
        );
        requests
            .owner
            .shutdown()
            .await
            .expect("#1450 registry fixture owner shutdown");
        requests
            .project
            .dispose()
            .await
            .expect("#1450 registry fixture project shutdown");
    }

    /// An old generation stays leased across reload. A stale callback must
    /// not target a new run even when all textual IDs are reused.
    #[tokio::test]
    async fn pinned_sdk_async_uses_completion_and_steer_identity_across_reload() {
        let (_scratch, requests) = crate::native_process::management_tests::setup();
        let session = JsString::from("sdk-pinned-identity");
        let run = JsString::from("same-run");
        let reservation1 = requests.owner.begin_turn(&session, &run).unwrap();
        let completion1 = requests.owner.begin_completion(&session, &run);
        let keep_old_generation = requests.project.acquire(None).unwrap();
        let old_steer = steer_resource(&requests.project);
        let binding1 = old_steer.bind_session(session.clone(), None).unwrap();
        let first = binding1
            .start_turn(
                "same-turn".into(),
                run.clone(),
                vec![],
                Cancellation::new(),
                &mut ModuleRouter::default(),
            )
            .unwrap();
        let registry =
            build_async_hook_registry(&requests.project, &requests.owner, &requests.coordinator)
                .unwrap();
        let guard1 = {
            let owner = requests.owner.clone();
            let session = session.clone();
            let completion = completion1.clone();
            Arc::new(move || owner.is_current_completion(&session, &completion))
        };
        let registration = |id: &str| AsyncHookRegistration {
            session_key: session.clone(),
            invocation_id: id.into(),
            hook_name: "hook".into(),
            hook_event: "PreToolUse".into(),
            timeout_ms: Some(60_000.0),
            include_hook_events: false,
        };
        let output = || {
            crate::metadata::object([(
                "hookSpecificOutput",
                crate::metadata::object([
                    ("hookEventName", "PreToolUse".into()),
                    ("additionalContext", "pinned context".into()),
                ]),
            )])
        };
        let pinned = || {
            crate::runtime::async_hooks::AsyncHookTurn::new(
                old_steer.active_steer_info(&session).unwrap(),
            )
        };
        registry
            .register_pinned(registration("live"), pinned(), guard1.clone())
            .unwrap();
        registry
            .register_pinned(registration("stale"), pinned(), guard1.clone())
            .unwrap();
        requests.project.reload().await.unwrap();
        let live = registry
            .submit(session.clone(), "live".into(), output())
            .await
            .unwrap();
        assert_eq!(live.status, AsyncHookStatus::Delivered);
        assert_eq!(
            old_steer
                .mailbox_for_test(&session)
                .unwrap()
                .drain(&"same-turn".into())
                .len(),
            1
        );
        first.close().await.unwrap();
        requests
            .owner
            .end_turn(&session, Some(&run), Some(&reservation1));
        let reservation2 = requests.owner.begin_turn(&session, &run).unwrap();
        let completion2 = requests.owner.begin_completion(&session, &run);
        let new_steer = steer_resource(&requests.project);
        let binding2 = new_steer.bind_session(session.clone(), None).unwrap();
        let second = binding2
            .start_turn(
                "same-turn".into(),
                run.clone(),
                vec![],
                Cancellation::new(),
                &mut ModuleRouter::default(),
            )
            .unwrap();
        assert!(!guard1());
        let result = registry
            .submit(session.clone(), "stale".into(), output())
            .await
            .unwrap();
        assert_eq!(result.status, AsyncHookStatus::Expired);
        assert!(
            new_steer
                .mailbox_for_test(&session)
                .unwrap()
                .drain(&"same-turn".into())
                .is_empty()
        );
        let guard2 = {
            let owner = requests.owner.clone();
            let session = session.clone();
            let completion = completion2.clone();
            Arc::new(move || owner.is_current_completion(&session, &completion))
        };
        registry
            .register_pinned(
                registration("fresh"),
                crate::runtime::async_hooks::AsyncHookTurn::new(
                    new_steer.active_steer_info(&session).unwrap(),
                ),
                guard2,
            )
            .unwrap();
        let fresh = registry
            .submit(session.clone(), "fresh".into(), output())
            .await
            .unwrap();
        assert_eq!(fresh.status, AsyncHookStatus::Delivered);
        assert_eq!(
            new_steer
                .mailbox_for_test(&session)
                .unwrap()
                .drain(&"same-turn".into())
                .len(),
            1
        );
        second.close().await.unwrap();
        requests
            .owner
            .end_turn(&session, Some(&run), Some(&reservation2));
        drop(keep_old_generation);
        requests.owner.shutdown().await.unwrap();
        requests.project.dispose().await.unwrap();
    }
    #[tokio::test]
    async fn legacy_async_hook_lookup_uses_session_project() {
        use crate::runtime::session::SessionContext;
        let (_scratch, requests) = crate::native_process::management_tests::setup();
        let b = requests
            .options
            .project
            .parent()
            .unwrap()
            .join("steer-hooks-B");
        std::fs::create_dir_all(&b).unwrap();
        let mut options = requests.options.clone();
        options.project = b.clone();
        super::super::register_project(&options).unwrap();
        let session = JsString::from("hook-project-b");
        requests.owner.enforce_project_binding();
        let _handle = requests
            .owner
            .get_or_create(SessionContext {
                session_key: session.clone(),
                project_key: Some(JsString::from(b.to_string_lossy().as_ref())),
                channel_key: "test".into(),
                allowed_tools: None,
                disallowed_tools: None,
            })
            .await
            .unwrap();
        let _reservation = requests.owner.begin_turn(&session, &"run".into()).unwrap();
        let lease = requests.project.acquire(Some(&b)).unwrap();
        let steer = lease
            .runtime()
            .resources()
            .get::<ProjectResources>()
            .unwrap()
            .context()
            .resource(Resource::InteractionSteer)
            .unwrap()
            .as_any()
            .downcast_ref::<InteractionSteer>()
            .unwrap()
            .clone();
        let binding = steer.bind_session(session.clone(), None).unwrap();
        let _turn = binding
            .start_turn(
                "turn".into(),
                "run".into(),
                vec![],
                Cancellation::new(),
                &mut ModuleRouter::default(),
            )
            .unwrap();
        let registry =
            build_async_hook_registry(&requests.project, &requests.owner, &requests.coordinator)
                .unwrap();
        registry
            .register(AsyncHookRegistration {
                session_key: session.clone(),
                invocation_id: "inv-b".into(),
                hook_name: "hook".into(),
                hook_event: "PreToolUse".into(),
                timeout_ms: Some(60_000.0),
                include_hook_events: false,
            })
            .unwrap();
        let output = crate::metadata::object([(
            "hookSpecificOutput",
            crate::metadata::object([
                ("hookEventName", "PreToolUse".into()),
                ("additionalContext", "B-hook".into()),
            ]),
        )]);
        let result = registry
            .submit(session.clone(), "inv-b".into(), output)
            .await
            .unwrap();
        assert_eq!(result.status, AsyncHookStatus::Delivered);
        let entries = steer
            .mailbox_for_test(&session)
            .unwrap()
            .drain(&"turn".into());
        assert!(!entries.is_empty(), "B turn received hook result");
        _turn.close().await.unwrap();
        requests.owner.end_turn(&session, Some(&"run".into()), Some(&_reservation));
        drop(binding);
        drop(lease);
        drop(_handle);
        tokio::time::timeout(std::time::Duration::from_secs(10), requests.owner.shutdown())
            .await.expect("B hook fixture owner cleanup must settle").unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(10), requests.project.dispose())
            .await.expect("B hook fixture project leases must be released").unwrap();
    }
}
