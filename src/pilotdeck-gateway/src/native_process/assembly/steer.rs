use super::{
    AsyncHookRegistry, AsyncHookTurn, JsValue, ProjectResources, ProjectRuntimeRegistry, Resource,
    SessionOwner, TokioHookClock,
};
use crate::runtime::project::interaction_hooks::AsyncHookSink;
use crate::runtime::turn::coordinator::TurnEventCoordinator;
use pilotdeck_protocol::JsString;
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
    let interaction_hooks = build_project.acquire(None).ok().and_then(|lease| {
        let context = lease
            .runtime()
            .resources()
            .get::<ProjectResources>()?
            .context();
        // The registration-side wiring needs the hooks resource; without
        // one, async hook outputs keep their local pending fallback and
        // `hook_async_result` answers `unknown` — the same degraded state
        // as a project without interaction hooks at all.
        let hooks = context
            .resource(Resource::InteractionHooks)?
            .as_any()
            .downcast_ref::<crate::runtime::project::interaction_hooks::InteractionHooks>()
            .cloned();
        context.resource(Resource::InteractionSteer).map(|_| hooks)
    });
    let interaction_hooks = interaction_hooks?;
    let reg_project = build_project.clone();
    let reg_owner = owner.clone();
    let reg_coordinator = coordinator.clone();
    let registry = Arc::new(AsyncHookRegistry::new(
        Arc::new(TokioHookClock),
        move |session_key| {
            // Turn is live only when the admission slot holds a run ID.
            let run_id = reg_owner.active_turn_run_id(session_key)?;
            // Shared lookup: fetch the InteractionSteer resource
            // and guard the run ID against the admission slot.
            let info = crate::runtime::project::interaction_steer::active_turn_for(
                &reg_project,
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
    ));
    // #1438 registration-side wiring: hand the registry to the hooks resource
    // so every turn binding bridges async hook outputs (invocation ids) into
    // it — the counterpart of TS ProjectSessionFactory.ts:231-243 passing
    // gateway.registerAsyncHook into the per-session HookRuntime.
    if let Some(hooks) = interaction_hooks {
        hooks.set_async_hook_sink(Arc::new(GatewayAsyncHookSink {
            registry: registry.clone(),
        }));
    }
    Some(registry)
}

/// The gateway's registration sink: resolves the session key at the turn
/// binding (the per-turn handler owns it) and forwards the registration.
/// `include_hook_events` keeps the TS default `false`: TS reads it from
/// `sdkSessionConfig?.includeHookEvents` (ProjectSessionRuntimeBundle.ts:382,
/// `options.includeHookEvents === true`), and this native slice has no SDK
/// session registry — `sdk_session_config` is not a wire method
/// (capabilities.rs, ledger U9-ASYNC-HOOK-BRIDGE-001).
struct GatewayAsyncHookSink {
    registry: Arc<AsyncHookRegistry>,
}
impl AsyncHookSink for GatewayAsyncHookSink {
    fn register(
        &self,
        session_key: &JsString,
        registration: crate::runtime::hooks::runtime::AsyncHookRegistration,
    ) -> Result<(), crate::host::HostError> {
        // The active-turn scoping, blank-id silent return, idempotent return
        // and the ASYNC_HOOK_INVOCATION_CONFLICT rejection all live in
        // `AsyncHookRegistry::register`, mirroring InProcessGateway.ts:699-731.
        self.registry
            .register(crate::runtime::async_hooks::AsyncHookRegistration {
                session_key: session_key.clone(),
                invocation_id: registration.invocation_id,
                hook_name: registration.hook_name,
                hook_event: registration.hook_event,
                timeout_ms: registration.timeout_ms,
                include_hook_events: false,
            })
    }
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

    /// The #1438 wiring shared by the four tests below: the production
    /// registry build (which now also installs the registration sink into the
    /// InteractionHooks resource) wired into a clone of the registry fixture.
    fn wired_requests(
        requests: &crate::native_process::requests::Requests,
    ) -> crate::native_process::requests::Requests {
        let registry =
            build_async_hook_registry(&requests.project, &requests.owner, &requests.coordinator)
                .expect("#1438 async hook registry");
        let mut wired = requests.clone();
        wired.async_hook_registry = Some(registry.clone());
        wired
    }

    /// A live steer turn plus its admission slot, the way a submitted run
    /// stages them. `finish` releases both before a restarted run may begin.
    struct LiveTurn {
        session: JsString,
        run_id: JsString,
        reservation: crate::runtime::session::TurnReservation,
        turn: crate::runtime::project::interaction_steer::TurnSteer,
    }
    impl LiveTurn {
        async fn live(
            requests: &crate::native_process::requests::Requests,
            steer: &InteractionSteer,
            session: &JsString,
            turn_id: &str,
            run_id: &str,
        ) -> LiveTurn {
            let reservation = requests
                .owner
                .begin_turn(session, &run_id.into())
                .expect("#1438 admission slot");
            let binding = steer
                .bind_session(session.clone(), None)
                .expect("#1438 steer session binding");
            let turn = binding
                .start_turn(
                    turn_id.into(),
                    run_id.into(),
                    vec![],
                    Cancellation::new(),
                    &mut ModuleRouter::default(),
                )
                .expect("#1438 steer turn start");
            LiveTurn {
                session: session.clone(),
                run_id: run_id.into(),
                reservation,
                turn,
            }
        }
        /// Close the turn and release the admission slot — the two steps a
        /// finished run performs before a retry restarts the turn.
        async fn finish(&self, requests: &crate::native_process::requests::Requests) {
            self.turn
                .close()
                .await
                .expect("#1438 steer turn closed and drained");
            requests
                .owner
                .end_turn(&self.session, Some(&self.run_id), Some(&self.reservation));
        }
    }

    /// Command-hook settings whose stdout is one SDK deferred-hook marker
    /// with invocation id `aid` (parse_hook_output's async branch). The
    /// default 60 s timeout keeps the pending entry alive for the whole test.
    fn async_hook_settings() -> crate::hooks::protocol::HooksSettings {
        crate::hooks::config::parse_hooks_config(&crate::metadata::object([(
            "PreToolUse",
            JsValue::Array(vec![crate::metadata::object([(
                "hooks",
                JsValue::Array(vec![crate::metadata::object([
                    ("type", "command".into()),
                    (
                        "command",
                        r#"/bin/echo '{"async":true,"asyncHookId":"aid"}'"#.into(),
                    ),
                ])]),
            )])]),
        )]))
        .settings
    }

    /// Bind the project's InteractionHooks resource for `run_id` with the
    /// async-echo command hook, exactly as InteractionTurn::bind does for a
    /// production turn (same resource, same bind_turn entry point).
    fn bound_hooks_turn(
        requests: &crate::native_process::requests::Requests,
        session: &JsString,
        run_id: &str,
    ) -> crate::runtime::project::interaction_hooks::TurnHooks {
        let lease = requests.project.acquire(None).expect("#1438 project lease");
        let hooks = lease
            .runtime()
            .resources()
            .get::<ProjectResources>()
            .expect("#1438 project resources")
            .context()
            .resource(Resource::InteractionHooks)
            .expect("#1438 interaction hooks resource")
            .as_any()
            .downcast_ref::<crate::runtime::project::interaction_hooks::InteractionHooks>()
            .expect("#1438 hooks resource type")
            .clone();
        drop(lease);
        hooks
            .bind_turn(
                crate::host::modules::lifecycle::Binding {
                    session_id: session.clone(),
                    turn_id: "t".into(),
                    cwd: std::env::temp_dir().to_string_lossy().into_owned().into(),
                    permission_mode: Some("default".into()),
                    base_env: None,
                    signal: None,
                },
                run_id.into(),
                async_hook_settings(),
                None,
                &mut ModuleRouter::default(),
            )
            .expect("#1438 hooks turn binding")
    }

    /// #1438 s3: the method-layer delivered positive control through the
    /// production registry — register a hook for a live turn, then submit its
    /// result over the real `hook_async_result` request path (never
    /// `registry.submit`), and require the non-empty context to land in the
    /// live run's steer mailbox.
    #[tokio::test]
    async fn hook_async_result_delivers_registered_context_through_the_method_layer() {
        use crate::runtime::turn::gateway::GatewayRequests;

        let (_scratch, fixture) =
            std::panic::catch_unwind(crate::native_process::management_tests::setup)
                .expect("#1438 registry fixture setup");
        let requests = wired_requests(&fixture);
        let session = JsString::from("method-delivery");
        let steer = steer_resource(&fixture.project);
        let _turn = LiveTurn::live(&requests, &steer, &session, "t", "r1").await;
        requests
            .async_hook_registry
            .clone()
            .expect("#1438 wired registry")
            .register(AsyncHookRegistration {
                session_key: session.clone(),
                invocation_id: "inv".into(),
                hook_name: "hook".into(),
                hook_event: "PreToolUse".into(),
                timeout_ms: Some(60_000.0),
                include_hook_events: false,
            })
            .expect("#1438 async hook registration");
        let result = requests
            .request(
                "hook_async_result".into(),
                crate::metadata::object([
                    ("sessionKey", JsValue::String(session.clone())),
                    ("invocationId", "inv".into()),
                    (
                        "output",
                        crate::metadata::object([(
                            "hookSpecificOutput",
                            crate::metadata::object([
                                ("hookEventName", "PreToolUse".into()),
                                ("additionalContext", "method context".into()),
                            ]),
                        )]),
                    ),
                ]),
            )
            .await
            .expect("#1438 method-layer submit stays Ok");
        crate::negative_assert_eq!(
            result.get("status").and_then(JsValue::as_string),
            Some(&JsString::from("delivered")),
            "#1438 the method layer reports delivered for a live turn's hook"
        );
        assert_eq!(
            result.get("invocationId").and_then(JsValue::as_string),
            Some(&JsString::from("inv")),
            "#1438 the method layer echoes the invocationId"
        );
        let drained = steer
            .mailbox_for_test(&session)
            .expect("#1438 session mailbox")
            .drain(&"t".into());
        assert_eq!(
            drained.len(),
            1,
            "#1438 the delivered context entered the live run's mailbox"
        );
        assert_eq!(
            drained[0].item_id,
            JsString::from("async-hook:inv"),
            "#1438 the delivered item carries the async-hook queue id"
        );
        let text = drained[0]
            .message
            .get("content")
            .and_then(|content| match content {
                JsValue::Array(items) => items.first(),
                _ => None,
            })
            .and_then(|item| item.get("text"))
            .and_then(JsValue::as_string)
            .expect("#1438 delivered steer carries the hook context");
        assert_eq!(
            text,
            &JsString::from(
                "<async_hook_context event=\"PreToolUse\">\nmethod context\n</async_hook_context>"
            ),
            "#1438 the delivered context keeps the TS wrapper shape"
        );
        fixture
            .owner
            .shutdown()
            .await
            .expect("#1438 fixture owner shutdown");
        fixture
            .project
            .dispose()
            .await
            .expect("#1438 fixture project shutdown");
    }

    /// #1438 s2: the registration wiring itself. After the production
    /// registry build installs the sink, a real hook dispatch of an async
    /// output (invocation id in stdout) must register in the gateway
    /// registry instead of the local pending list — observable because the
    /// method-layer submit answers `delivered`, and the dispatch result's
    /// `pending_async_hooks` stays empty. A revert of the wiring to
    /// `on_async_registered: None` fails both assertions (submit would answer
    /// `unknown` and the descriptor would surface locally).
    #[tokio::test]
    async fn production_hook_dispatch_registers_async_output_in_the_gateway_registry() {
        use crate::host::modules::lifecycle::LifecyclePort as _;
        use crate::runtime::turn::gateway::GatewayRequests;

        let (_scratch, fixture) =
            std::panic::catch_unwind(crate::native_process::management_tests::setup)
                .expect("#1438 registry fixture setup");
        let requests = wired_requests(&fixture);
        let session = JsString::from("wiring");
        let steer = steer_resource(&fixture.project);
        let _turn = LiveTurn::live(&requests, &steer, &session, "t", "r1").await;
        let hooks_turn = bound_hooks_turn(&requests, &session, "r1");
        let dispatch = hooks_turn
            .dispatch(crate::runtime::hooks::LifecycleDispatchInput {
                event: "PreToolUse".into(),
                base_input: JsObject::new(),
                payload: None,
                match_query: None,
                cwd: std::env::temp_dir().to_string_lossy().into_owned().into(),
                env: None,
                signal: None,
            })
            .await
            .expect("#1438 hook dispatch with a bridged registration stays Ok");
        // Receipt-wired form of the assert (#1438 negative controls M1/M2
        // bind this exact oracle): the text and the pass/fail condition are
        // unchanged.
        crate::negative_assert!(
            dispatch.pending_async_hooks.is_empty(),
            "#1438 a bridged async output must not enter the local pending list"
        );
        let result = requests
            .request(
                "hook_async_result".into(),
                crate::metadata::object([
                    ("sessionKey", JsValue::String(session.clone())),
                    ("invocationId", "aid".into()),
                    (
                        "output",
                        crate::metadata::object([(
                            "hookSpecificOutput",
                            crate::metadata::object([
                                ("hookEventName", "PreToolUse".into()),
                                ("additionalContext", "wired context".into()),
                            ]),
                        )]),
                    ),
                ]),
            )
            .await
            .expect("#1438 method-layer submit stays Ok");
        crate::negative_assert_eq!(
            result.get("status").and_then(JsValue::as_string),
            Some(&JsString::from("delivered")),
            "#1438 the dispatched async output reached the gateway registry"
        );
        let drained = steer
            .mailbox_for_test(&session)
            .expect("#1438 session mailbox")
            .drain(&"t".into());
        assert_eq!(
            drained.len(),
            1,
            "#1438 the wired registration delivered through the real enqueue"
        );
        assert_eq!(
            drained[0].item_id,
            JsString::from("async-hook:aid"),
            "#1438 the delivered item carries the dispatched invocation id"
        );
        fixture
            .owner
            .shutdown()
            .await
            .expect("#1438 fixture owner shutdown");
        fixture
            .project
            .dispose()
            .await
            .expect("#1438 fixture project shutdown");
    }

    /// #1438 s2: the conflict path through the production wiring. TS
    /// `registerAsyncHook` throws `ASYNC_HOOK_INVOCATION_CONFLICT` when the
    /// invocation id is already owned by another active turn
    /// (InProcessGateway.ts:724-729) and `HookRuntime.run` propagates the
    /// throw (HookRuntime.ts:170). After run r1 registers `aid`, a restarted
    /// run r2 dispatching the same id must fail its hook dispatch with that
    /// code — swallowing the error (an infallible bridge) is the deviation.
    #[tokio::test]
    async fn production_async_hook_conflict_fails_the_dispatch() {
        use crate::host::modules::lifecycle::LifecyclePort as _;

        let (_scratch, fixture) =
            std::panic::catch_unwind(crate::native_process::management_tests::setup)
                .expect("#1438 registry fixture setup");
        let requests = wired_requests(&fixture);
        let session = JsString::from("conflict");
        let steer = steer_resource(&fixture.project);
        let first = LiveTurn::live(&requests, &steer, &session, "t", "r1").await;
        let hooks_turn = bound_hooks_turn(&requests, &session, "r1");
        hooks_turn
            .dispatch(crate::runtime::hooks::LifecycleDispatchInput {
                event: "PreToolUse".into(),
                base_input: JsObject::new(),
                payload: None,
                match_query: None,
                cwd: std::env::temp_dir().to_string_lossy().into_owned().into(),
                env: None,
                signal: None,
            })
            .await
            .expect("#1438 first dispatch registers its async output");
        // r1 ends; a retry restarts the turn under a new run id.
        first.finish(&requests).await;
        let _restart = LiveTurn::live(&requests, &steer, &session, "t", "r2").await;
        let second = bound_hooks_turn(&requests, &session, "r2");
        let dispatch_result = second
            .dispatch(crate::runtime::hooks::LifecycleDispatchInput {
                event: "PreToolUse".into(),
                base_input: JsObject::new(),
                payload: None,
                match_query: None,
                cwd: std::env::temp_dir().to_string_lossy().into_owned().into(),
                env: None,
                signal: None,
            })
            .await;
        // Receipt-wired form of expect_err (#1438 negative control M3 binds
        // this exact oracle): the text and the pass/fail condition are
        // unchanged.
        let error = crate::test_negative_receipt::expect_err(
            dispatch_result,
            "#1438 a conflicting invocation id must fail the dispatch",
        );
        assert_eq!(
            error
                .module_error
                .as_deref()
                .and_then(|m| m.code.as_ref())
                .and_then(|c| c.to_utf8().ok()),
            Some("ASYNC_HOOK_INVOCATION_CONFLICT".to_owned()),
            "#1438 the conflict surfaces with the TS domain code"
        );
        fixture
            .owner
            .shutdown()
            .await
            .expect("#1438 fixture owner shutdown");
        fixture
            .project
            .dispose()
            .await
            .expect("#1438 fixture project shutdown");
    }

    /// #1438 s3 (§2.3 裁决的行为钉): TS does not expire pending async hooks
    /// when the turn closes (`expireAsyncHooksForTurn` has no caller; probe
    /// `ts-probe/output.txt` steps 3-5), so the Rust registry must not
    /// either. Observable pin: after r1 closes, the pending `inv-close` is
    /// still owned by r1 — r2 cannot register the same id (conflict), and
    /// the method-layer submit settles it as `expired` through the
    /// submit-refusal path, delivering nothing to the restarted run.
    #[tokio::test]
    async fn production_async_hook_pending_survives_turn_close_until_submit() {
        use crate::runtime::turn::gateway::GatewayRequests;

        let (_scratch, fixture) =
            std::panic::catch_unwind(crate::native_process::management_tests::setup)
                .expect("#1438 registry fixture setup");
        let requests = wired_requests(&fixture);
        let session = JsString::from("close-pin");
        let steer = steer_resource(&fixture.project);
        let first = LiveTurn::live(&requests, &steer, &session, "t", "r1").await;
        requests
            .async_hook_registry
            .clone()
            .expect("#1438 wired registry")
            .register(AsyncHookRegistration {
                session_key: session.clone(),
                invocation_id: "inv-close".into(),
                hook_name: "hook".into(),
                hook_event: "PreToolUse".into(),
                timeout_ms: Some(60_000.0),
                include_hook_events: false,
            })
            .expect("#1438 async hook registration");
        // r1 ends; the retry restarts under a new run id.
        first.finish(&requests).await;
        let _restart = LiveTurn::live(&requests, &steer, &session, "t", "r2").await;
        // The pending entry survived the close: the restarted run cannot
        // claim the same invocation id. (If a close-side expiry existed, this
        // would register cleanly instead — the TS probe's step 5.) Receipt-
        // wired expect_err: the §2.3 pin a close-side expiry deviation would
        // have to break, with the text and condition unchanged.
        let register_result = requests
            .async_hook_registry
            .clone()
            .expect("#1438 wired registry")
            .register(AsyncHookRegistration {
                session_key: session.clone(),
                invocation_id: "inv-close".into(),
                hook_name: "hook".into(),
                hook_event: "PreToolUse".into(),
                timeout_ms: Some(60_000.0),
                include_hook_events: false,
            });
        let conflict = crate::test_negative_receipt::expect_err(
            register_result,
            "#1438 the closed turn still owns its invocation id",
        );
        assert_eq!(
            conflict
                .module_error
                .as_deref()
                .and_then(|m| m.code.as_ref())
                .and_then(|c| c.to_utf8().ok()),
            Some("ASYNC_HOOK_INVOCATION_CONFLICT".to_owned()),
            "#1438 the surviving pending entry rejects a re-registration"
        );
        // Submit-refusal expiry, exactly the TS probe's step 6/7.
        let result = requests
            .request(
                "hook_async_result".into(),
                crate::metadata::object([
                    ("sessionKey", JsValue::String(session.clone())),
                    ("invocationId", "inv-close".into()),
                    (
                        "output",
                        crate::metadata::object([(
                            "hookSpecificOutput",
                            crate::metadata::object([
                                ("hookEventName", "PreToolUse".into()),
                                ("additionalContext", "too late".into()),
                            ]),
                        )]),
                    ),
                ]),
            )
            .await
            .expect("#1438 method-layer submit stays Ok");
        crate::negative_assert_eq!(
            result.get("status").and_then(JsValue::as_string),
            Some(&JsString::from("expired")),
            "#1438 a closed turn's pending entry settles expired on submit"
        );
        let drained = steer
            .mailbox_for_test(&session)
            .expect("#1438 session mailbox")
            .drain(&"t".into());
        assert!(
            drained.is_empty(),
            "#1438 the restarted run received nothing"
        );
        fixture
            .owner
            .shutdown()
            .await
            .expect("#1438 fixture owner shutdown");
        fixture
            .project
            .dispose()
            .await
            .expect("#1438 fixture project shutdown");
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
}
