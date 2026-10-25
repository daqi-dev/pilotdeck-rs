use super::*;
use crate::host::modules::lifecycle::LifecyclePort as _;
use crate::runtime::hooks::exec::{ShellFuture, ShellPort, ShellRequest, ShellResult};
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

struct BlockingHookShell {
    log: Arc<std::sync::Mutex<Vec<String>>>,
    started: Arc<tokio::sync::Notify>,
    release: Arc<tokio::sync::Notify>,
    honor_cancel: bool,
}
impl ShellPort for BlockingHookShell {
    fn execute(&self, request: ShellRequest) -> ShellFuture {
        let log = self.log.clone();
        let started = self.started.clone();
        let release = self.release.clone();
        let honor_cancel = self.honor_cancel;
        Box::pin(async move {
            let input = parse_json(&String::from_utf16_lossy(request.stdin.units())).unwrap();
            let tag = input
                .get("tag")
                .and_then(JsValue::as_string)
                .unwrap()
                .to_utf8()
                .unwrap();
            log.lock().unwrap().push(format!("start:{tag}"));
            if tag == "old" {
                started.notify_one();
                if honor_cancel {
                    if let Some(signal) = request.signal {
                        signal.cancelled().await;
                        log.lock().unwrap().push("cancelled:old".into());
                    }
                } else {
                    // Deliberately ignore cancellation: this models a hook
                    // process that has already started and will finish on its
                    // own schedule. The barrier, not this gate, orders it.
                    release.notified().await;
                }
            }
            log.lock().unwrap().push(format!("end:{tag}"));
            Ok(ShellResult {
                exit_code: Some(0),
                stdout: vec![],
                stderr: vec![],
                timed_out: false,
            })
        })
    }
}
fn gate_options(shell: Arc<BlockingHookShell>) -> HookRuntimeOptions {
    HookRuntimeOptions {
        shell: Some(shell),
        ..Default::default()
    }
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

#[test]
fn replacement_ts_oracle_records_unserialized_same_key_overlap() {
    // This is the measured cc7c4e6 fact the port-only barrier deliberately
    // strengthens: the TS host reuses one session runtime, so a second hook
    // item may overlap the first, and a replacement runtime starts before the
    // old runtime has drained. The Rust barrier is not claimed to be TS.
    let oracle = json(include_str!(
        "../../../../tests/fixtures/interaction-hooks/replacement-oracle.json"
    ));
    assert_eq!(
        oracle.get("commit").and_then(JsValue::as_string),
        Some(&"cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85".into())
    );
    let observations = oracle.get("observations").unwrap();
    let same = observations.get("same_session_runtime").unwrap();
    assert_eq!(
        same.get("same_session_key_reused"),
        Some(&JsValue::Bool(true))
    );
    assert_eq!(
        same.get("second_started_before_first_end"),
        Some(&JsValue::Bool(true))
    );
    let replacement = observations.get("replacement").unwrap();
    assert_eq!(
        replacement.get("old_state_while_inflight"),
        Some(&"draining".into())
    );
    assert_eq!(
        replacement.get("replacement_started_before_old_drain"),
        Some(&JsValue::Bool(true))
    );
    assert_eq!(
        replacement.get("after_cleanup_old_rejected"),
        Some(&"Cannot run hooks; hook runtime is disposed.".into())
    );
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
async fn same_key_rebind_waits_for_old_cleanup_before_new_hook_runs() {
    struct GateShell {
        log: Arc<std::sync::Mutex<Vec<String>>>,
        old_started: Arc<tokio::sync::Notify>,
        release: Arc<tokio::sync::Notify>,
        new_started: Arc<tokio::sync::Notify>,
    }
    impl ShellPort for GateShell {
        fn execute(&self, request: ShellRequest) -> ShellFuture {
            let log = self.log.clone();
            let old_started = self.old_started.clone();
            let release = self.release.clone();
            let new_started = self.new_started.clone();
            Box::pin(async move {
                let input = parse_json(&String::from_utf16_lossy(request.stdin.units())).unwrap();
                let tag = input
                    .get("tag")
                    .and_then(JsValue::as_string)
                    .unwrap()
                    .to_utf8()
                    .unwrap();
                log.lock().unwrap().push(format!("start:{tag}"));
                if tag == "old" {
                    old_started.notify_one();
                    // Deliberately ignore cancellation: this models a hook
                    // process that has already started and will finish on its
                    // own schedule. The barrier, not this gate, must order it.
                    release.notified().await;
                } else {
                    new_started.notify_one();
                }
                log.lock().unwrap().push(format!("end:{tag}"));
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
    let log = Arc::new(std::sync::Mutex::new(Vec::new()));
    let old_started = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let new_started = Arc::new(tokio::sync::Notify::new());
    let options = || HookRuntimeOptions {
        shell: Some(Arc::new(GateShell {
            log: log.clone(),
            old_started: old_started.clone(),
            release: release.clone(),
            new_started: new_started.clone(),
        })),
        ..Default::default()
    };
    let mut old_router = ModuleRouter::default();
    let _old = resource
        .bind_with_options(
            binding(),
            "old-run".into(),
            async_settings(),
            None,
            &mut old_router,
            options(),
        )
        .unwrap();
    let old_call =
        tokio::spawn(old_router.module_call(call("PreToolUse", json(r#"{"tag":"old"}"#))));
    tokio::time::timeout(Duration::from_secs(2), old_started.notified())
        .await
        .expect("old hook did not start");
    let mut new_router = ModuleRouter::default();
    let new = resource
        .bind_with_options(
            binding(),
            "new-run".into(),
            async_settings(),
            None,
            &mut new_router,
            options(),
        )
        .unwrap();
    assert_eq!(lock(&resource.state).draining.len(), 1);
    let mut new_call =
        Box::pin(new_router.module_call(call("PreToolUse", json(r#"{"tag":"new"}"#))));
    std::future::poll_fn(|cx| {
        assert!(
            std::future::Future::poll(new_call.as_mut(), cx).is_pending(),
            "new hook dispatch completed before old cleanup"
        );
        std::task::Poll::Ready(())
    })
    .await;
    assert!(
        tokio::time::timeout(Duration::from_secs(1), new_started.notified())
            .await
            .is_err(),
        "new hook started before the old run cleanup barrier"
    );
    release.notify_one();
    tokio::time::timeout(Duration::from_secs(2), new_call)
        .await
        .expect("new hook did not finish after the barrier")
        .unwrap();
    assert_eq!(
        *log.lock().unwrap(),
        ["start:old", "end:old", "start:new", "end:new"]
    );
    assert!(
        tokio::time::timeout(Duration::from_secs(2), old_call)
            .await
            .expect("old dispatch did not return")
            .unwrap()
            .is_err()
    );
    assert!(lock(&resource.state).draining.is_empty());
    assert!(lock(&resource.state).turns.contains_key(&"s".into()));
    drop(new);
    resource.dispose().await.unwrap();
}

#[tokio::test]
async fn dropped_stream_then_same_key_rebind_waits_for_old_cleanup() {
    let resource = InteractionHooks::new(InteractionPermission::default());
    let log = Arc::new(std::sync::Mutex::new(Vec::new()));
    let started = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let shell = Arc::new(BlockingHookShell {
        log: log.clone(),
        started: started.clone(),
        release: release.clone(),
        honor_cancel: false,
    });
    let mut old_router = ModuleRouter::default();
    let old = resource
        .bind_with_options(
            binding(),
            "old-run".into(),
            async_settings(),
            None,
            &mut old_router,
            gate_options(shell.clone()),
        )
        .unwrap();
    let old_call =
        tokio::spawn(old_router.module_call(call("PreToolUse", json(r#"{"tag":"old"}"#))));
    tokio::time::timeout(Duration::from_secs(2), started.notified())
        .await
        .expect("old hook did not start");
    // Dropping the consumer stream closes this turn's hooks identity; the
    // detached cleanup is still waiting for the started hook job.
    old.close();
    let mut new_router = ModuleRouter::default();
    let new = resource
        .bind_with_options(
            binding(),
            "new-run".into(),
            async_settings(),
            None,
            &mut new_router,
            gate_options(shell),
        )
        .unwrap();
    assert_eq!(lock(&resource.state).draining.len(), 1);
    let mut ready = Box::pin(new.wait_ready());
    std::future::poll_fn(|cx| {
        assert!(
            std::future::Future::poll(ready.as_mut(), cx).is_pending(),
            "replacement became ready while old hook cleanup was pending"
        );
        std::task::Poll::Ready(())
    })
    .await;
    release.notify_one();
    tokio::time::timeout(Duration::from_secs(2), ready)
        .await
        .expect("replacement did not pass the cleanup barrier")
        .unwrap();
    new_router
        .module_call(call("PreToolUse", json(r#"{"tag":"new"}"#)))
        .await
        .unwrap();
    assert_eq!(
        *log.lock().unwrap(),
        ["start:old", "end:old", "start:new", "end:new"]
    );
    assert!(
        tokio::time::timeout(Duration::from_secs(2), old_call)
            .await
            .unwrap()
            .unwrap()
            .is_err()
    );
    resource.dispose().await.unwrap();
}

#[tokio::test]
async fn same_key_rebind_after_cleanup_dispatches_immediately() {
    let resource = InteractionHooks::new(InteractionPermission::default());
    let log = Arc::new(std::sync::Mutex::new(Vec::new()));
    let started = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let shell = Arc::new(BlockingHookShell {
        log: log.clone(),
        started: started.clone(),
        release: release.clone(),
        honor_cancel: false,
    });
    let mut old_router = ModuleRouter::default();
    let old = resource
        .bind_with_options(
            binding(),
            "old-run".into(),
            async_settings(),
            None,
            &mut old_router,
            gate_options(shell.clone()),
        )
        .unwrap();
    let old_call =
        tokio::spawn(old_router.module_call(call("PreToolUse", json(r#"{"tag":"old"}"#))));
    tokio::time::timeout(Duration::from_secs(2), started.notified())
        .await
        .expect("old hook did not start");
    release.notify_one();
    old.close();
    tokio::time::timeout(Duration::from_secs(2), old.wait_closed())
        .await
        .expect("old cleanup did not finish")
        .unwrap();
    let mut new_router = ModuleRouter::default();
    let new = resource
        .bind_with_options(
            binding(),
            "new-run".into(),
            async_settings(),
            None,
            &mut new_router,
            gate_options(shell),
        )
        .unwrap();
    tokio::time::timeout(Duration::from_secs(1), new.wait_ready())
        .await
        .expect("a cleaned predecessor must not block the replacement")
        .unwrap();
    new_router
        .module_call(call("PreToolUse", json(r#"{"tag":"new"}"#)))
        .await
        .unwrap();
    assert_eq!(
        *log.lock().unwrap(),
        ["start:old", "end:old", "start:new", "end:new"]
    );
    assert!(
        tokio::time::timeout(Duration::from_secs(2), old_call)
            .await
            .unwrap()
            .unwrap()
            .is_err()
    );
    resource.dispose().await.unwrap();
}

#[tokio::test]
async fn canceled_predecessor_releases_same_key_barrier() {
    let resource = InteractionHooks::new(InteractionPermission::default());
    let log = Arc::new(std::sync::Mutex::new(Vec::new()));
    let started = Arc::new(tokio::sync::Notify::new());
    let shell = Arc::new(BlockingHookShell {
        log: log.clone(),
        started: started.clone(),
        release: Arc::new(tokio::sync::Notify::new()),
        honor_cancel: true,
    });
    let mut old_router = ModuleRouter::default();
    let _old = resource
        .bind_with_options(
            binding(),
            "old-run".into(),
            async_settings(),
            None,
            &mut old_router,
            gate_options(shell.clone()),
        )
        .unwrap();
    let old_call =
        tokio::spawn(old_router.module_call(call("PreToolUse", json(r#"{"tag":"old"}"#))));
    tokio::time::timeout(Duration::from_secs(2), started.notified())
        .await
        .expect("old hook did not start");
    let mut new_router = ModuleRouter::default();
    let new = resource
        .bind_with_options(
            binding(),
            "new-run".into(),
            async_settings(),
            None,
            &mut new_router,
            gate_options(shell),
        )
        .unwrap();
    tokio::time::timeout(Duration::from_secs(2), new.wait_ready())
        .await
        .expect("canceled predecessor did not release the barrier")
        .unwrap();
    new_router
        .module_call(call("PreToolUse", json(r#"{"tag":"new"}"#)))
        .await
        .unwrap();
    assert_eq!(
        *log.lock().unwrap(),
        [
            "start:old",
            "cancelled:old",
            "end:old",
            "start:new",
            "end:new"
        ]
    );
    assert!(
        tokio::time::timeout(Duration::from_secs(2), old_call)
            .await
            .unwrap()
            .unwrap()
            .is_err()
    );
    resource.dispose().await.unwrap();
}

#[tokio::test]
async fn superseded_replacement_cancel_terminates_never_finishing_predecessor() {
    let resource = InteractionHooks::new(InteractionPermission::default());
    let log = Arc::new(std::sync::Mutex::new(Vec::new()));
    let started = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let shell = Arc::new(BlockingHookShell {
        log: log.clone(),
        started: started.clone(),
        release: release.clone(),
        honor_cancel: false,
    });
    let mut old_router = ModuleRouter::default();
    let old = resource
        .bind_with_options(
            binding(),
            "old-run".into(),
            async_settings(),
            None,
            &mut old_router,
            gate_options(shell.clone()),
        )
        .unwrap();
    let old_call =
        tokio::spawn(old_router.module_call(call("PreToolUse", json(r#"{"tag":"old"}"#))));
    tokio::time::timeout(Duration::from_secs(2), started.notified())
        .await
        .expect("old hook did not start");
    let signal = Cancellation::new();
    let mut bound = binding();
    bound.signal = Some(signal.clone());
    let mut new_router = ModuleRouter::default();
    let new = resource
        .bind_with_options(
            bound,
            "new-run".into(),
            async_settings(),
            None,
            &mut new_router,
            gate_options(shell),
        )
        .unwrap();
    let mut ready = Box::pin(new.wait_ready());
    std::future::poll_fn(|cx| {
        assert!(std::future::Future::poll(ready.as_mut(), cx).is_pending());
        std::task::Poll::Ready(())
    })
    .await;
    signal.cancel();
    let error = tokio::time::timeout(Duration::from_secs(2), ready)
        .await
        .expect("cancelled replacement waited on a never-finishing predecessor")
        .unwrap_err();
    assert!(error.to_string().contains("superseded"));
    assert!(
        lock(&resource.state)
            .draining
            .iter()
            .any(|(_, run)| Arc::ptr_eq(run, &old.run))
    );
    release.notify_one();
    tokio::time::timeout(Duration::from_secs(2), old_call)
        .await
        .expect("old dispatch did not unwind")
        .unwrap()
        .unwrap_err();
    resource.dispose().await.unwrap();
}

#[tokio::test]
async fn failed_cleanup_blocks_same_key_only() {
    let resource = InteractionHooks::new(InteractionPermission::default());
    let log = Arc::new(std::sync::Mutex::new(Vec::new()));
    let started = Arc::new(tokio::sync::Notify::new());
    let shell = Arc::new(BlockingHookShell {
        log: log.clone(),
        started,
        release: Arc::new(tokio::sync::Notify::new()),
        honor_cancel: false,
    });
    let old = resource
        .bind_with_options(
            binding(),
            "old-run".into(),
            async_settings(),
            None,
            &mut ModuleRouter::default(),
            gate_options(shell.clone()),
        )
        .unwrap();
    old.run.runtime.hooks().poison_cleanup_bus_for_test();
    let replacement = resource
        .bind_with_options(
            binding(),
            "new-run".into(),
            async_settings(),
            None,
            &mut ModuleRouter::default(),
            gate_options(shell.clone()),
        )
        .unwrap();
    assert!(
        tokio::time::timeout(Duration::from_secs(2), replacement.wait_ready())
            .await
            .expect("failed cleanup did not settle")
            .is_err(),
        "failed predecessor cleanup granted a fresh hook barrier"
    );
    assert!(
        replacement
            .dispatch(LifecycleDispatchInput {
                event: "PreToolUse".into(),
                base_input: JsObject::new(),
                payload: None,
                match_query: None,
                cwd: "".into(),
                env: None,
                signal: None,
            })
            .await
            .is_err()
    );
    let latest = resource
        .bind_with_options(
            binding(),
            "latest-run".into(),
            async_settings(),
            None,
            &mut ModuleRouter::default(),
            gate_options(shell.clone()),
        )
        .unwrap();
    assert!(
        tokio::time::timeout(Duration::from_secs(2), latest.wait_ready())
            .await
            .expect("failed cleanup was forgotten on rebind")
            .is_err()
    );
    let mut other = binding();
    other.session_id = "other".into();
    let independent = resource
        .bind_with_options(
            other,
            "other-run".into(),
            async_settings(),
            None,
            &mut ModuleRouter::default(),
            gate_options(shell),
        )
        .unwrap();
    tokio::time::timeout(Duration::from_secs(2), independent.wait_ready())
        .await
        .expect("another session key must not wait on this barrier")
        .unwrap();
    assert!(resource.dispose().await.is_err());
}

#[tokio::test]
async fn dropped_dispatch_drains_and_keeps_host_environment() {
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
