use super::*;
use crate::{
    config::permission::RuleSetHandle,
    hooks::config::parse_hooks_config,
    host::{
        HostHandler,
        modules::capability::{ToolCatalog, ToolDefinition},
    },
    runtime::{
        interaction::{GatewayInteractionCoordinator, PermissionDecision},
        permission,
        project::{
            InteractionProfileName, RuntimeProfile, RuntimeProfileOverrides,
            interaction_permission::PermissionTurnOptions,
        },
    },
};
use pilotdeck_protocol::parse_json;
use std::time::Duration;

fn json(text: &str) -> JsValue {
    parse_json(text).unwrap()
}
fn object(fields: &[(&str, JsValue)]) -> JsValue {
    let mut result = JsObject::new();
    for (key, value) in fields {
        result.insert((*key).into(), value.clone());
    }
    JsValue::Object(result)
}
fn binding() -> lifecycle::Binding {
    lifecycle::Binding {
        session_id: "s".into(),
        turn_id: "t".into(),
        cwd: env!("CARGO_MANIFEST_DIR").into(),
        permission_mode: Some("default".into()),
        base_env: Some(
            json(r#"{"PATH":"/usr/bin:/bin"}"#)
                .as_object()
                .unwrap()
                .clone(),
        ),
        signal: None,
    }
}

#[test]
fn interaction_hooks_cleanup_runtime_shutdown_is_observable() {
    for close_before_shutdown in [false, true] {
        let executor = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let resource = InteractionHooks::new(InteractionPermission::default());
        let turn = {
            let _entered = executor.enter();
            let turn = resource
                .bind_turn(
                    binding(),
                    "run".into(),
                    vec![],
                    None,
                    &mut ModuleRouter::default(),
                )
                .unwrap();
            if close_before_shutdown {
                turn.close();
            }
            turn
        };
        drop(executor);
        turn.close();
        let observer = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        observer.block_on(async {
            let error = tokio::time::timeout(Duration::from_secs(3), turn.wait_closed())
                .await
                .unwrap()
                .unwrap_err();
            assert!(error.to_string().contains("cleanup task"));
            assert!(resource.dispose().await.is_err());
            assert!(resource.dispose().await.is_err());
        });
    }
}

#[tokio::test]
async fn interaction_hooks_nested_cleanup_panic_is_observable() {
    let resource = InteractionHooks::new(InteractionPermission::default());
    let turn = resource
        .bind_turn(
            binding(),
            "run".into(),
            vec![],
            None,
            &mut ModuleRouter::default(),
        )
        .unwrap();
    turn.run.runtime.hooks().poison_cleanup_bus_for_test();
    turn.close();
    let error = tokio::time::timeout(Duration::from_secs(3), turn.wait_closed())
        .await
        .unwrap()
        .unwrap_err();
    assert!(error.to_string().contains("cleanup task"));
    assert!(resource.dispose().await.is_err());
    assert!(resource.dispose().await.is_err());
    assert_eq!(turn.release_count(), 0);
}

#[tokio::test]
async fn interaction_hooks_synchronous_cleanup_panic_is_observable() {
    let permission = InteractionPermission::default();
    let resource = InteractionHooks::new(permission.clone());
    let port = bind_permission(
        &permission,
        Arc::default(),
        Arc::new(|_| true),
        RuleSetHandle::empty(),
        InteractionProfileName::Interactive,
    );
    let turn = resource
        .bind_turn(
            binding(),
            "same".into(),
            vec![],
            Some(port),
            &mut ModuleRouter::default(),
        )
        .unwrap();
    assert!(turn.run.registration.is_some());
    turn.run
        .runtime
        .hooks()
        .callback_executor()
        .poison_state_for_test();
    turn.close(); // no unwind, and closed=true must have an error completion
    let error = tokio::time::timeout(Duration::from_secs(3), turn.wait_closed())
        .await
        .unwrap()
        .unwrap_err();
    assert!(error.to_string().contains("cleanup task"));
    assert!(resource.dispose().await.is_err());
}
fn call(event: &str, payload: JsValue) -> JsValue {
    object(&[
        ("module", "lifecycle".into()),
        (
            "payload",
            object(&[
                ("operation", "dispatch".into()),
                ("event", event.into()),
                ("payload", payload),
            ]),
        ),
    ])
}
fn settings(event: &str, command: &str) -> HooksSettings {
    parse_hooks_config(&object(&[(
        event,
        JsValue::Array(vec![object(&[(
            "hooks",
            JsValue::Array(vec![object(&[
                ("type", "command".into()),
                ("command", command.into()),
                ("asyncRewake", JsValue::Bool(true)),
            ])]),
        )])]),
    )]))
    .settings
}
fn async_settings() -> HooksSettings {
    settings("PreToolUse", "printf '%s' '{\"async\":true}'")
}
fn completion() -> AsyncHookCompletion {
    AsyncHookCompletion {
        stdout: Some("{\"continue\":false,\"stopReason\":\"stop\"}".into()),
        ..Default::default()
    }
}
fn bind(resource: &InteractionHooks) -> (TurnHooks, ModuleRouter) {
    let mut router = ModuleRouter::default();
    let turn = resource
        .bind_with_options(
            binding(),
            "same".into(),
            async_settings(),
            None,
            &mut router,
            HookRuntimeOptions {
                clock: Arc::new(|| "<time>".into()),
                ..Default::default()
            },
        )
        .unwrap();
    (turn, router)
}
async fn pending(turn: &TurnHooks, router: &ModuleRouter) -> HookReceipt {
    router
        .module_call(call("PreToolUse", json("{}")))
        .await
        .unwrap();
    turn.pending_receipts().unwrap().pop().unwrap()
}
struct Catalog;
impl ToolCatalog for Catalog {
    fn list(&self) -> HostFuture<Vec<Arc<dyn ToolDefinition>>> {
        Box::pin(async { Ok(vec![]) })
    }
}
fn bind_permission(
    resource: &InteractionPermission,
    coordinator: Arc<GatewayInteractionCoordinator>,
    emit: Arc<dyn Fn(JsValue) -> bool + Send + Sync>,
    rules: RuleSetHandle,
    profile: InteractionProfileName,
) -> TurnPermission {
    resource
        .bind_turn(
            "s".into(),
            "same".into(),
            PermissionTurnOptions {
                profile: RuntimeProfile::resolve(
                    &JsValue::Undefined,
                    &RuntimeProfileOverrides {
                        interaction_profile_override: Some(profile),
                        ..Default::default()
                    },
                )
                .interaction,
                context: permission::create_default_permission_context(
                    permission::DefaultPermissionContextOptions {
                        cwd: env!("CARGO_MANIFEST_DIR").into(),
                        can_prompt: Some(true),
                        ..Default::default()
                    },
                ),
                rules,
                coordinator,
                emit,
                timeout: Duration::from_secs(5),
            },
            Arc::new(Catalog),
            &mut ModuleRouter::default(),
        )
        .unwrap()
}

#[tokio::test]
async fn configured_hooks_and_permission_callback_match_real_ts() {
    let oracle = json(include_str!(
        "../../../../tests/fixtures/interaction-hooks/oracle.json"
    ));
    let JsValue::Array(cases) = oracle.get("cases").unwrap() else {
        panic!()
    };
    assert_eq!(cases.len(), 3);
    for row in cases {
        let mode = row.get("mode").unwrap().as_string().unwrap().clone();
        let permission = InteractionPermission::default();
        let resource = InteractionHooks::new(permission.clone());
        let rules = RuleSetHandle::empty();
        let coordinator = Arc::new(GatewayInteractionCoordinator::default());
        let answerer = coordinator.clone();
        let turn_permission = bind_permission(
            &permission,
            coordinator,
            Arc::new(move |event| {
                assert_eq!(event.get("toolName"), Some(&"danger".into()));
                assert_eq!(event.get("payload"), Some(&json(r#"{"arg":1}"#)));
                if mode.equals_str("no-sink") {
                    return false;
                }
                answerer.decide_permission(
                    &"s".into(),
                    PermissionDecision {
                        request_id: event.get("requestId").unwrap().as_string().unwrap().clone(),
                        decision: if mode.equals_str("deny") {
                            "deny"
                        } else {
                            "allow"
                        }
                        .into(),
                        remember: Some(JsValue::Bool(true)),
                        reason: Some("blocked".into()),
                    },
                    None,
                )
            }),
            rules.clone(),
            InteractionProfileName::Interactive,
        );
        let mut router = ModuleRouter::default();
        let turn = resource
            .bind_turn(
                binding(),
                "same".into(),
                settings(
                    "PermissionRequest",
                    "printf '%s' '{\"systemMessage\":\"configured\"}'",
                ),
                Some(turn_permission),
                &mut router,
            )
            .unwrap();
        assert!(router.module_inventory(&Default::default()).lifecycle);
        let payload = object(&[
            ("toolName", "danger".into()),
            ("toolUseId", "fallback-call".into()),
            ("input", json(r#"{"arg":1}"#)),
            (
                "permissionSuggestions",
                oracle.get("suggestions").unwrap().clone(),
            ),
        ]);
        let result = router
            .module_call(call("PermissionRequest", payload))
            .await
            .unwrap();
        assert_eq!(result.get("result"), row.get("result"));
        assert_eq!(
            JsValue::Array(
                rules
                    .snapshot()
                    .allow
                    .iter()
                    .map(PermissionRule::to_value)
                    .collect()
            ),
            *row.get("rules").unwrap()
        );
        resource.dispose().await.unwrap();
        assert_eq!(turn.release_count(), 1);
    }
}

#[tokio::test]
async fn async_receipts_match_ts_and_reject_duplicates_old_runs_and_generations() {
    let oracle = json(include_str!(
        "../../../../tests/fixtures/interaction-hooks/oracle.json"
    ));
    let expected = oracle.get("async").unwrap();
    let resource = InteractionHooks::new(InteractionPermission::default());
    let (old, old_router) = bind(&resource);
    let result = old_router
        .module_call(call("PreToolUse", json("{}")))
        .await
        .unwrap();
    assert_eq!(result.get("result"), expected.get("pending"));
    let receipt = old.pending_receipts().unwrap().pop().unwrap();
    let mut wrong_generation = receipt.clone();
    wrong_generation.descriptor.generation += 1;
    assert!(!old.complete_async_hook(&wrong_generation, completion()));
    assert!(!old.cancel_async_hook(&wrong_generation));
    assert_eq!(
        JsValue::Bool(old.complete_async_hook(&receipt, completion())),
        *expected.get("accepted").unwrap()
    );
    assert_eq!(
        JsValue::Bool(old.complete_async_hook(&receipt, completion())),
        *expected.get("duplicate").unwrap()
    );
    assert_eq!(
        JsValue::Array(
            old.collect_async_responses()
                .unwrap()
                .iter()
                .map(AsyncHookResponse::to_value)
                .collect()
        ),
        *expected.get("responses").unwrap()
    );
    assert!(old.collect_async_responses().unwrap().is_empty());
    let old_receipt = pending(&old, &old_router).await;
    let (current, router) = bind(&resource);
    let current_receipt = pending(&current, &router).await;
    assert!(!old.complete_async_hook(&old_receipt, completion()));
    assert!(!current.complete_async_hook(&old_receipt, completion()));
    assert!(
        old_router
            .module_call(call("PreToolUse", json("{}")))
            .await
            .is_err()
    );
    old.close();
    old.wait_closed().await.unwrap();
    assert_eq!(old.release_count(), 1);
    let other_generation = InteractionHooks::new(InteractionPermission::default());
    let (new, new_router) = bind(&other_generation);
    let new_receipt = pending(&new, &new_router).await;
    assert_eq!(current_receipt.descriptor(), new_receipt.descriptor());
    assert!(!new.complete_async_hook(&current_receipt, completion()));
    assert!(!new.cancel_async_hook(&current_receipt));
    assert!(current.cancel_async_hook(&current_receipt));
    assert!(!current.cancel_async_hook(&current_receipt));
    assert!(!current.complete_async_hook(&current_receipt, completion()));
    assert!(current.collect_async_responses().unwrap().is_empty());
    resource.dispose().await.unwrap();
    resource.dispose().await.unwrap();
    assert!(resource.is_closed());
    assert_eq!(resource.release_count(), 1);
    assert!(current.pending_receipts().is_err());
    assert!(current.collect_async_responses().is_err());
    assert!(new.complete_async_hook(&new_receipt, completion()));
    other_generation.dispose().await.unwrap();
    assert!(!new.complete_async_hook(&new_receipt, completion()));
    assert!(
        resource
            .bind_turn(
                binding(),
                "again".into(),
                vec![],
                None,
                &mut ModuleRouter::default()
            )
            .is_err()
    );
}

#[tokio::test]
async fn permission_wait_is_cancelled_once_and_foreign_permission_is_rejected() {
    let permission = InteractionPermission::default();
    let resource = InteractionHooks::new(permission.clone());
    let coordinator = Arc::new(GatewayInteractionCoordinator::default());
    let (send, mut receive) = tokio::sync::mpsc::unbounded_channel();
    let rules = RuleSetHandle::empty();
    let port = bind_permission(
        &permission,
        coordinator.clone(),
        Arc::new(move |event| {
            send.send(event).unwrap();
            true
        }),
        rules.clone(),
        InteractionProfileName::Interactive,
    );
    let signal = Cancellation::new();
    let mut bound = binding();
    bound.signal = Some(signal.clone());
    let mut router = ModuleRouter::default();
    let turn = resource
        .bind_turn(
            bound,
            "same".into(),
            vec![],
            Some(port.clone()),
            &mut router,
        )
        .unwrap();
    let reused = resource.bind_turn(
        binding(),
        "same".into(),
        vec![],
        Some(port.clone()),
        &mut ModuleRouter::default(),
    );
    assert!(reused.is_err());
    let foreign = InteractionHooks::new(InteractionPermission::default());
    assert!(
        foreign
            .bind_turn(
                binding(),
                "same".into(),
                vec![],
                Some(port),
                &mut ModuleRouter::default()
            )
            .is_err()
    );
    let request = tokio::spawn(
        router.module_call(call("PermissionRequest", json(r#"{"toolName":"danger"}"#))),
    );
    let event = tokio::time::timeout(Duration::from_secs(2), receive.recv())
        .await
        .unwrap()
        .unwrap();
    signal.cancel();
    assert!(
        tokio::time::timeout(Duration::from_secs(2), request)
            .await
            .unwrap()
            .unwrap()
            .is_err()
    );
    turn.close();
    router.dispose().await.unwrap();
    tokio::time::timeout(Duration::from_secs(2), turn.wait_closed())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(turn.release_count(), 1);
    assert!(!coordinator.decide_permission(
        &"s".into(),
        PermissionDecision {
            request_id: event.get("requestId").unwrap().as_string().unwrap().clone(),
            decision: "allow".into(),
            remember: Some(JsValue::Bool(true)),
            reason: None,
        },
        None
    ));
    assert!(rules.snapshot().allow.is_empty());
    resource.dispose().await.unwrap();
    resource.dispose().await.unwrap();
    assert_eq!(resource.release_count(), 1);
}

#[tokio::test]
async fn profiles_do_not_install_gateway_callback_and_handler_dispose_revokes_run() {
    for profile in [
        InteractionProfileName::Disabled,
        InteractionProfileName::Headless,
    ] {
        let permission = InteractionPermission::default();
        let resource = InteractionHooks::new(permission.clone());
        let port = bind_permission(
            &permission,
            Arc::default(),
            Arc::new(|_| panic!("must not prompt")),
            RuleSetHandle::empty(),
            profile,
        );
        let mut router = ModuleRouter::default();
        let turn = resource
            .bind_turn(binding(), "same".into(), vec![], Some(port), &mut router)
            .unwrap();
        let result = router
            .module_call(call("PermissionRequest", json("{}")))
            .await
            .unwrap();
        assert_eq!(
            result.get("result"),
            Some(&LifecycleDispatchResult::default().to_value())
        );
        // Synchronous revoke remains effective if disposal's future is dropped.
        drop(router.dispose());
        assert!(
            router
                .module_call(call("PermissionRequest", json("{}")))
                .await
                .is_err()
        );
        turn.wait_closed().await.unwrap();
        assert_eq!(turn.release_count(), 1);
        resource.dispose().await.unwrap();
    }
}

#[tokio::test]
async fn dropped_dispatch_drains_and_keeps_host_environment() {
    use crate::runtime::hooks::exec::{ShellFuture, ShellPort, ShellRequest};
    struct BlockingShell(Arc<tokio::sync::Notify>, Arc<AtomicUsize>);
    impl ShellPort for BlockingShell {
        fn execute(&self, request: ShellRequest) -> ShellFuture {
            let ready = self.0.clone();
            let drops = self.1.clone();
            Box::pin(async move {
                assert_eq!(request.cwd, JsString::from(env!("CARGO_MANIFEST_DIR")));
                assert_eq!(
                    request.env.unwrap().get("PILOTDECK_SESSION_ID"),
                    Some(&"s".into())
                );
                struct Guard(Arc<AtomicUsize>);
                impl Drop for Guard {
                    fn drop(&mut self) {
                        self.0.fetch_add(1, Ordering::SeqCst);
                    }
                }
                let _guard = Guard(drops);
                ready.notify_one();
                request.signal.unwrap().cancelled().await;
                Ok(crate::runtime::hooks::exec::ShellResult {
                    exit_code: Some(0),
                    stdout: vec![],
                    stderr: vec![],
                    timed_out: false,
                })
            })
        }
    }
    let resource = InteractionHooks::new(InteractionPermission::default());
    let ready = Arc::new(tokio::sync::Notify::new());
    let drops = Arc::new(AtomicUsize::new(0));
    let mut router = ModuleRouter::default();
    let turn = resource
        .bind_with_options(
            binding(),
            "same".into(),
            async_settings(),
            None,
            &mut router,
            HookRuntimeOptions {
                shell: Some(Arc::new(BlockingShell(ready.clone(), drops.clone()))),
                ..Default::default()
            },
        )
        .unwrap();
    let request = tokio::spawn(router.module_call(call(
        "PreToolUse",
        json(r#"{"sessionId":"forged","cwd":"/forged"}"#),
    )));
    tokio::time::timeout(Duration::from_secs(2), ready.notified())
        .await
        .unwrap();
    request.abort();
    assert!(
        tokio::time::timeout(Duration::from_secs(2), request)
            .await
            .unwrap()
            .unwrap_err()
            .is_cancelled()
    );
    tokio::time::timeout(Duration::from_secs(2), turn.wait_closed())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(drops.load(Ordering::SeqCst), 1);
    assert_eq!(turn.release_count(), 1);
    resource.dispose().await.unwrap();
}

#[tokio::test]
async fn replacing_permission_revokes_hooks_before_next_hooks_bind() {
    use crate::host::modules::permission::PermissionContext;
    let permission = InteractionPermission::default();
    let resource = InteractionHooks::new(permission.clone());
    let bind_permission = || {
        bind_permission(
            &permission,
            Arc::default(),
            Arc::new(|_| false),
            RuleSetHandle::empty(),
            InteractionProfileName::Interactive,
        )
    };
    let old_permission = bind_permission();
    let mut router = ModuleRouter::default();
    let turn = resource
        .bind_turn(
            binding(),
            "same".into(),
            async_settings(),
            Some(old_permission),
            &mut router,
        )
        .unwrap();
    let receipt = pending(&turn, &router).await;
    let current_permission = bind_permission();
    assert!(!turn.complete_async_hook(&receipt, completion()));
    assert!(!turn.cancel_async_hook(&receipt));
    assert!(
        router
            .module_call(call("PreToolUse", json("{}")))
            .await
            .is_err()
    );
    tokio::time::timeout(Duration::from_secs(2), turn.wait_closed())
        .await
        .unwrap()
        .unwrap();
    assert!(current_permission.prepare(JsValue::Undefined).await.is_ok());
    let weak = Arc::downgrade(&turn.run);
    drop(turn);
    drop(router);
    drop(receipt);
    // One scheduler yield lets the monitor release its final run handle.
    tokio::task::yield_now().await;
    assert!(weak.upgrade().is_none());
    assert!(lock(&resource.state).turns.is_empty());
    assert!(lock(&resource.state).draining.is_empty());
    resource.dispose().await.unwrap();
    permission.close();
}

#[cfg(unix)]
#[tokio::test]
async fn real_shell_cancel_runs_term_trap_before_runtime_drain() {
    let root = std::env::temp_dir().join(format!("pdrs-hook-term-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&root).unwrap();
    let permission = InteractionPermission::default();
    let resource = InteractionHooks::new(permission);
    let mut bound = binding();
    bound.cwd = root.to_str().unwrap().into();
    let mut router = ModuleRouter::default();
    let turn = resource.bind_turn(bound, "same".into(), settings("PreToolUse",
        "trap 'printf done > terminated; exit 0' TERM; printf ready > ready; while :; do sleep 0.01; done"),
        None, &mut router).unwrap();
    let request = tokio::spawn(router.module_call(call("PreToolUse", json("{}"))));
    let ready = tokio::time::timeout(Duration::from_secs(3), async {
        while !root.join("ready").exists() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await;
    turn.close();
    let result = tokio::time::timeout(Duration::from_secs(2), request).await;
    let drained = tokio::time::timeout(Duration::from_secs(10), turn.wait_closed()).await;
    let terminated = std::fs::read_to_string(root.join("terminated"));
    std::fs::remove_dir_all(&root).unwrap();
    ready.unwrap();
    assert!(result.unwrap().unwrap().is_err());
    drained.unwrap().unwrap();
    assert_eq!(terminated.unwrap(), "done");
    assert_eq!(turn.release_count(), 1);
    resource.dispose().await.unwrap();
}
