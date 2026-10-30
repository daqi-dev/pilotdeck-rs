use super::super::{
    GatewayCommandCatalogBundle, LoadedPlugin, PluginRuntime, PluginRuntimeOptions, PluginSource,
};
use super::tests::{Temp, fixture};
use super::*;
use serde_json::Value;
use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

struct Injected(Arc<dyn Fn() -> BoxCommandsListFuture + Send + Sync>);
impl CommandCatalogListCommands for Injected {
    fn list(
        &self,
        _: CommandsListInput,
        _: String,
        _: Vec<PluginCommandContribution>,
    ) -> BoxCommandsListFuture {
        (self.0)()
    }
}
#[tokio::test]
async fn catalog_lease_finally() {
    const MSG: &str = "#887 catalog lease finally";
    for list_fails in [false, true] {
        for release_fails in [false, true] {
            let temp = Temp::new();
            let disposed = Arc::new(AtomicUsize::new(0));
            let count = disposed.clone();
            let events = Arc::new(std::sync::Mutex::new(vec![]));
            let release_events = events.clone();
            let release_entered = Arc::new(tokio::sync::Notify::new());
            let release_gate = Arc::new(tokio::sync::Notify::new());
            let released = release_entered.clone();
            let finish = release_gate.clone();
            let plugin = Arc::new(LoadedPlugin {
                name: "lease".into(),
                path: temp.0.join("missing"),
                source: PluginSource::Builtin,
                commands: vec![],
                skills: vec![],
                output_styles: vec![],
                hooks_config: None,
                mcp_servers: None,
                lsp_servers: None,
                dispose: Some(Box::new(move || {
                    count.fetch_add(1, Ordering::SeqCst);
                    release_events.lock().expect(MSG).push("release");
                    released.notify_one();
                    let finish = finish.clone();
                    Box::pin(async move {
                        finish.notified().await;
                        if release_fails {
                            Err("release failed".into())
                        } else {
                            Ok(())
                        }
                    })
                })),
            });
            let runtime = Arc::new(PluginRuntime::new(PluginRuntimeOptions {
                project_root: temp.0.join("project"),
                pilot_home: temp.0.join("home"),
                builtin_skills_root: None,
                builtin_plugins: vec![plugin],
                builtin_plugins_enabled: Default::default(),
            }));
            let injected_runtime = runtime.clone();
            let (entered, mut entered_rx) = tokio::sync::mpsc::unbounded_channel();
            let gate = Arc::new(tokio::sync::Notify::new());
            let gate_clone = gate.clone();
            let list_events = events.clone();
            let provider = Injected(Arc::new(move || {
                list_events.lock().expect(MSG).push("list");
                let runtime = injected_runtime.clone();
                let entered = entered.clone();
                let gate = gate_clone.clone();
                Box::pin(async move {
                    assert_eq!(
                        runtime
                            .state
                            .lock()
                            .await
                            .registry
                            .lease_counts
                            .values()
                            .sum::<usize>(),
                        1,
                        "{MSG}: acquired before list"
                    );
                    // Retire the exact leased generation so release has observable disposal work.
                    {
                        let mut state = runtime.state.lock().await;
                        let replacement = state.registry.replace_all(vec![]).expect(MSG);
                        super::super::start_disposals(
                            &mut state.registry,
                            replacement.retired,
                            &runtime.state,
                        );
                    }
                    entered.send(()).expect(MSG);
                    gate.notified().await;
                    if list_fails {
                        Err(PluginRuntimeError::new("list failed"))
                    } else {
                        Ok(CommandsListResult { value: object([]) })
                    }
                })
            }));
            let warnings = Arc::new(AtomicUsize::new(0));
            let warn = warnings.clone();
            let warn_events = events.clone();
            let resolved = runtime.clone();
            let bundle = GatewayCommandCatalogBundle::new(
                temp.0.join("home").to_string_lossy().into_owned(),
                Arc::new(|p| Box::pin(async move { Ok(p) })),
                Arc::new(move |_| resolved.clone()),
                Arc::new(provider),
                Some(Arc::new(move |_, e| {
                    assert_eq!(e.message, "release failed", "{MSG}: warning");
                    warn_events.lock().expect(MSG).push("warn");
                    warn.fetch_add(1, Ordering::SeqCst);
                })),
            );
            let task = tokio::spawn(async move {
                bundle
                    .commands_list(CommandsListInput {
                        project_key: "project".into(),
                        query: None,
                        cursor: None,
                        limit: None,
                    })
                    .await
            });
            tokio::time::timeout(Duration::from_secs(2), entered_rx.recv())
                .await
                .expect(MSG)
                .expect(MSG);
            assert!(!task.is_finished(), "{MSG}: pending list");
            assert_eq!(
                disposed.load(Ordering::SeqCst),
                0,
                "{MSG}: no early release"
            );
            gate.notify_one();
            tokio::time::timeout(Duration::from_secs(2), release_entered.notified())
                .await
                .expect(MSG);
            assert!(!task.is_finished(), "{MSG}: release must be awaited");
            release_gate.notify_one();
            let result = tokio::time::timeout(Duration::from_secs(2), task)
                .await
                .expect(MSG)
                .expect(MSG);
            if list_fails {
                assert_eq!(
                    result.expect_err(MSG).message,
                    "list failed",
                    "{MSG}: preserve list error"
                );
            } else {
                assert!(result.is_ok(), "{MSG}: preserve success");
            }
            assert_eq!(
                disposed.load(Ordering::SeqCst),
                1,
                "{MSG}: release exactly once"
            );
            assert_eq!(
                warnings.load(Ordering::SeqCst),
                usize::from(release_fails),
                "{MSG}: warning count"
            );
            assert!(
                runtime.state.lock().await.registry.lease_counts.is_empty(),
                "{MSG}: released generation"
            );
            let mode = match (list_fails, release_fails) {
                (false, false) => "success",
                (true, false) => "list-error",
                (false, true) => "release-error",
                (true, true) => "both-errors",
            };
            let oracle = fixture();
            let expected = oracle["lifecycle"]
                .as_array()
                .expect(MSG)
                .iter()
                .find(|c| c["mode"] == mode)
                .expect(MSG);
            let projected: Vec<_> = expected["events"]
                .as_array()
                .expect(MSG)
                .iter()
                .filter_map(Value::as_str)
                .filter(|e| ["list", "release", "warn"].contains(e))
                .collect();
            assert_eq!(
                *events.lock().expect(MSG),
                projected,
                "{MSG}: TS lifecycle order"
            );
            if list_fails {
                assert_eq!(expected["error"], "list failed", "{MSG}: TS list failure");
            }
            for pending_mode in ["list-pending", "release-pending"] {
                let expected = oracle["lifecycle"]
                    .as_array()
                    .expect(MSG)
                    .iter()
                    .find(|c| c["mode"] == pending_mode)
                    .expect(MSG);
                assert_eq!(
                    expected["settled"], false,
                    "{MSG}: TS waits at deterministic barrier"
                );
            }
            assert_eq!(
                expected["events"]
                    .as_array()
                    .expect(MSG)
                    .iter()
                    .filter(|e| e.as_str() == Some("release"))
                    .count(),
                disposed.load(Ordering::SeqCst),
                "{MSG}: real TS release count"
            );
        }
    }
}

#[tokio::test]
async fn catalog_before_acquire_failures() {
    use super::super::{PluginRefreshResult, RegistryState};
    const MSG: &str = "#887 catalog before acquire";
    for mode in ["resolve-error", "refresh-error", "acquire-error"] {
        let temp = Temp::new();
        let runtime = Arc::new(PluginRuntime::new(PluginRuntimeOptions {
            project_root: temp.0.join("project"),
            pilot_home: temp.0.join("home"),
            builtin_skills_root: None,
            builtin_plugins: vec![],
            builtin_plugins_enabled: Default::default(),
        }));
        let (tx, rx) = tokio::sync::watch::channel(None);
        runtime.state.lock().await.refresh_receiver = Some(rx);
        if mode == "refresh-error" {
            tx.send(Some(Err(PluginRuntimeError::new("refresh failed"))))
                .expect(MSG);
        }
        let listed = Arc::new(AtomicUsize::new(0));
        let counter = listed.clone();
        let resolved = runtime.clone();
        let bundle = GatewayCommandCatalogBundle::new(
            "home".into(),
            Arc::new(move |p| {
                Box::pin(async move {
                    if mode == "resolve-error" {
                        Err(PluginRuntimeError::new("resolve failed"))
                    } else {
                        Ok(p)
                    }
                })
            }),
            Arc::new(move |_| resolved.clone()),
            Arc::new(Injected(Arc::new(move || {
                counter.fetch_add(1, Ordering::SeqCst);
                Box::pin(async { Ok(CommandsListResult { value: object([]) }) })
            }))),
            Some(Arc::new(|_, _| {
                panic!("#887 catalog before acquire: unexpected release warning")
            })),
        );
        let mut pending = Box::pin(bundle.commands_list(CommandsListInput {
            project_key: "project".into(),
            query: None,
            cursor: None,
            limit: None,
        }));
        if mode == "acquire-error" {
            // Existing single-flight refresh receiver is a deterministic boundary hook.
            use std::{
                future::Future,
                task::{Context, Poll, Waker},
            };
            assert!(
                matches!(
                    pending
                        .as_mut()
                        .poll(&mut Context::from_waker(Waker::noop())),
                    Poll::Pending
                ),
                "{MSG}: waiting refresh"
            );
            runtime.state.lock().await.registry.state = RegistryState::Draining;
            tx.send(Some(Ok(PluginRefreshResult {
                generation: 0,
                previous: vec![],
                next: vec![],
                added: vec![],
                removed: vec![],
                staged_failures: vec![],
            })))
            .expect(MSG);
        }
        let error = tokio::time::timeout(Duration::from_secs(2), pending)
            .await
            .expect(MSG)
            .expect_err(MSG);
        let oracle = fixture();
        let expected = oracle["lifecycle"]
            .as_array()
            .expect(MSG)
            .iter()
            .find(|c| c["mode"] == mode)
            .expect(MSG);
        if mode == "acquire-error" {
            assert_eq!(
                error.message, "Cannot acquire plugins; plugin registry is draining.",
                "{MSG}"
            );
        } else {
            assert_eq!(
                error.message,
                expected["error"].as_str().expect(MSG),
                "{MSG}"
            );
        }
        assert_eq!(listed.load(Ordering::SeqCst), 0, "{MSG}: list must not run");
        assert!(
            !expected["events"]
                .as_array()
                .expect(MSG)
                .iter()
                .any(|e| e == "list" || e == "release"),
            "{MSG}: TS pre-acquire branch"
        );
        assert!(
            runtime.state.lock().await.registry.lease_counts.is_empty(),
            "{MSG}: no lease"
        );
    }
}

// All three public leases can leave the task that acquired them. Test each
// flavor separately so its Drop implementation has its own negative binding.
async fn lease_drop_off_runtime(flavor: &str) {
    const MSG: &str = "#1010 plugin lease off-runtime";
    let temp = Temp::new();
    let plugin = Arc::new(LoadedPlugin {
        name: "off-runtime".into(),
        path: temp.0.join("missing"),
        source: PluginSource::Builtin,
        commands: vec![],
        skills: vec![],
        output_styles: vec![],
        hooks_config: None,
        mcp_servers: None,
        lsp_servers: None,
        dispose: None,
    });
    let runtime = Arc::new(PluginRuntime::new(PluginRuntimeOptions {
        project_root: temp.0.join("project"),
        pilot_home: temp.0.join("home"),
        builtin_skills_root: None,
        builtin_plugins: vec![plugin],
        builtin_plugins_enabled: Default::default(),
    }));
    runtime.refresh().await.expect(MSG);
    let lease: Box<dyn Send> = match flavor {
        "command" => Box::new(runtime.acquire_command_catalog().await.expect(MSG)),
        "session" => Box::new(
            runtime
                .acquire_session_contributions(Vec::new())
                .await
                .expect(MSG),
        ),
        "legacy" => Box::new(runtime.acquire_contribution_snapshot().await.expect(MSG)),
        _ => panic!("#1010 invalid plugin lease test flavor"),
    };
    let count = || async {
        runtime
            .state
            .lock()
            .await
            .registry
            .lease_counts
            .values()
            .sum::<usize>()
    };
    assert_eq!(count().await, 1, "#1010 plugin lease acquired once");
    let (send, receive) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            assert!(
                tokio::runtime::Handle::try_current().is_err(),
                "#1010 plugin drop has no ambient runtime"
            );
            drop(lease);
        }));
        let _ = send.send(result);
    });
    receive
        .recv_timeout(Duration::from_secs(2))
        .expect("#1010 plugin drop thread completes within its bound")
        .expect("#1010 plugin lease drop without a runtime does not panic");
    tokio::time::timeout(Duration::from_secs(2), async {
        while count().await != 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("#1010 captured executor releases the plugin lease");
    runtime.dispose().await.expect(MSG);
    assert_eq!(count().await, 0, "#1010 plugin lease remains released");
}
#[tokio::test]
async fn command_lease_drop_without_runtime() {
    lease_drop_off_runtime("command").await;
}
#[tokio::test]
async fn session_lease_drop_without_runtime() {
    lease_drop_off_runtime("session").await;
}
#[tokio::test]
async fn legacy_lease_drop_without_runtime() {
    lease_drop_off_runtime("legacy").await;
}

fn shutdown_executor(executor: tokio::runtime::Runtime) {
    let (sent, received) = std::sync::mpsc::channel();
    let thread = std::thread::spawn(move || {
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| drop(executor)));
        let _ = sent.send(outcome);
    });
    received
        .recv_timeout(Duration::from_secs(3))
        .expect("#1010 executor shutdown finishes without waiting for state")
        .expect("#1010 executor shutdown does not panic");
    thread
        .join()
        .expect("#1010 finished shutdown thread joined");
}

// #1010: no filesystem/provider work; exercise the real registry, acquisition,
// Drop/release and disposal entry points with one observable builtin callback.
async fn shutdown_lease_fixture() -> (Arc<PluginRuntime>, Arc<AtomicUsize>) {
    let calls = Arc::new(AtomicUsize::new(0));
    let observed = calls.clone();
    let plugin = Arc::new(LoadedPlugin {
        name: "shutdown-1010".into(),
        path: "/virtual/1010/plugin".into(),
        source: PluginSource::Builtin,
        commands: vec![],
        skills: vec![],
        output_styles: vec![],
        hooks_config: None,
        mcp_servers: None,
        lsp_servers: None,
        dispose: Some(Box::new(move || {
            observed.fetch_add(1, Ordering::SeqCst);
            Box::pin(async { Ok(()) })
        })),
    });
    let runtime = Arc::new(PluginRuntime::new(PluginRuntimeOptions {
        project_root: "/virtual/1010/project".into(),
        pilot_home: "/virtual/1010/home".into(),
        builtin_skills_root: None,
        builtin_plugins: vec![],
        builtin_plugins_enabled: Default::default(),
    }));
    runtime
        .state
        .lock()
        .await
        .registry
        .replace_all(vec![plugin])
        .expect("#1010 shutdown fixture registry active");
    (runtime, calls)
}

async fn shutdown_lease_count(runtime: &PluginRuntime) -> usize {
    runtime
        .state
        .lock()
        .await
        .registry
        .lease_counts
        .values()
        .sum()
}

async fn dead_executor_lease(flavor: &str) {
    for queued in [false, true] {
        let (runtime, calls) = shutdown_lease_fixture().await;
        let executor = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("#1010 plugin shutdown executor");
        let lease: Box<dyn Send> = {
            let _entered = executor.enter();
            match flavor {
                "command" => Box::new(
                    runtime
                        .acquire_command_catalog()
                        .await
                        .expect("#1010 command shutdown lease"),
                ),
                "session" => Box::new(
                    runtime
                        .acquire_session_contributions(vec![])
                        .await
                        .expect("#1010 session shutdown lease"),
                ),
                "legacy" => Box::new(
                    runtime
                        .acquire_contribution_snapshot()
                        .await
                        .expect("#1010 legacy shutdown lease"),
                ),
                _ => panic!("#1010 unknown shutdown lease flavor"),
            }
        };
        assert_eq!(
            shutdown_lease_count(&runtime).await,
            1,
            "#1010 shutdown lease held"
        );
        let mut lease = Some(lease);
        if queued {
            drop(lease.take());
        }
        shutdown_executor(executor);
        drop(lease);
        let error = tokio::time::timeout(Duration::from_secs(3), runtime.dispose())
            .await
            .expect("#1010 abandoned plugin lease disposal completes boundedly")
            .expect_err("#1010 abandoned plugin lease cannot report success");
        assert_eq!(
            error.message, "Plugin lease release stopped before releasing its references.",
            "#1010 abandoned plugin lease reports explicit failure"
        );
        let repeated = tokio::time::timeout(Duration::from_secs(3), runtime.dispose())
            .await
            .expect("#1010 abandoned plugin lease repeated disposal bounded")
            .expect_err("#1010 abandoned plugin lease repeated error");
        assert_eq!(
            repeated.message, error.message,
            "#1010 abandoned lease failure is sticky"
        );
        assert_eq!(
            calls.load(Ordering::SeqCst),
            0,
            "#1010 abandoned lease skips no live references"
        );
        assert_eq!(
            shutdown_lease_count(&runtime).await,
            1,
            "#1010 abandoned lease does not fake cleanup"
        );
    }
}

#[tokio::test]
async fn dead_executor_command_lease_reports_failure() {
    dead_executor_lease("command").await;
}
#[tokio::test]
async fn dead_executor_session_lease_reports_failure() {
    dead_executor_lease("session").await;
}
#[tokio::test]
async fn dead_executor_legacy_lease_reports_failure() {
    dead_executor_lease("legacy").await;
}

use super::super::contributions::{
    LeaseReleaseGuard, PluginCommandCatalogLease, PluginContributionLease,
    PluginSessionContributionLease,
};
use std::future::Future;

enum ShutdownLease {
    Command(PluginCommandCatalogLease),
    Session(PluginSessionContributionLease),
    Legacy(PluginContributionLease),
}
impl ShutdownLease {
    async fn acquire(runtime: &PluginRuntime, flavor: &str) -> Self {
        match flavor {
            "command" => Self::Command(
                runtime
                    .acquire_command_catalog()
                    .await
                    .expect("#1010 explicit command acquisition"),
            ),
            "session" => Self::Session(
                runtime
                    .acquire_session_contributions(vec![])
                    .await
                    .expect("#1010 explicit session acquisition"),
            ),
            "legacy" => Self::Legacy(
                runtime
                    .acquire_contribution_snapshot()
                    .await
                    .expect("#1010 explicit legacy acquisition"),
            ),
            _ => panic!("#1010 explicit unknown flavor"),
        }
    }
    async fn release(&mut self) -> Result<(), PluginRuntimeError> {
        match self {
            Self::Command(lease) => lease.release().await,
            Self::Session(lease) => lease.release().await,
            Self::Legacy(lease) => lease.release().await,
        }
    }
}

#[tokio::test]
async fn explicit_release_shutdown_reports_failure() {
    for flavor in ["command", "session", "legacy"] {
        let (runtime, _) = shutdown_lease_fixture().await;
        let executor = tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("#1010 explicit release executor");
        let mut lease = ShutdownLease::acquire(&runtime, flavor).await;
        {
            let _entered = executor.enter();
            let mut release = Box::pin(lease.release());
            assert!(
                release
                    .as_mut()
                    .poll(&mut std::task::Context::from_waker(std::task::Waker::noop()))
                    .is_pending(),
                "#1010 explicit release scheduled before poll"
            );
            // Cancel the caller, not the owned release task.
        }
        shutdown_executor(executor);
        let error = tokio::time::timeout(Duration::from_secs(3), lease.release())
            .await
            .expect("#1010 repeated lost release returns boundedly")
            .expect_err("#1010 repeated lost release returns error");
        assert_eq!(
            error.message, "Plugin lease release task was dropped or panicked before completion.",
            "#1010 explicit release preserves its own completed failure"
        );
        drop(lease);
        let error = tokio::time::timeout(Duration::from_secs(3), runtime.dispose())
            .await
            .expect("#1010 explicit lost release disposal bounded")
            .expect_err("#1010 explicit lost release disposal error");
        assert_eq!(
            error.message, "Plugin lease release stopped before releasing its references.",
            "#1010 explicit release cannot strand disposal"
        );
    }
}

#[tokio::test]
async fn polled_lease_release_shutdown_while_state_locked() {
    let (runtime, _) = shutdown_lease_fixture().await;
    let mut lease = runtime
        .acquire_command_catalog()
        .await
        .expect("#1010 lock-wait lease");
    // Transfer this real acquired lease into the exact future used by all
    // release tasks; the polling wrapper observes Pending without changing it.
    lease.released = true;
    let guard = LeaseReleaseGuard::new(lease.failure.clone(), !lease.plugins.is_empty());
    let executor = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("#1010 lock-wait executor");
    let held = runtime.state.lock().await;
    let (entered, mut observed) = tokio::sync::watch::channel(false);
    executor.spawn(async move {
        let release = super::super::release_lease_plugins(
            &lease.plugins,
            &lease.state,
            &lease.waiters,
            guard,
        );
        tokio::pin!(release);
        std::future::poll_fn(|cx| {
            let polled = release.as_mut().poll(cx);
            if polled.is_pending() {
                entered.send_replace(true);
            }
            polled
        })
        .await
    });
    let executor = std::thread::spawn(move || {
        executor.block_on(async {
            tokio::time::timeout(Duration::from_secs(3), observed.wait_for(|v| *v))
                .await
                .expect("#1010 release reached held state lock")
                .expect("#1010 release lock gate alive");
        });
        executor
    })
    .join()
    .expect("#1010 drive lock-wait executor");
    shutdown_executor(executor);
    let error = tokio::time::timeout(Duration::from_secs(3), runtime.dispose())
        .await
        .expect("#1010 lost release bypasses held registry lock")
        .expect_err("#1010 lock-wait lost release error");
    assert_eq!(
        error.message, "Plugin lease release stopped before releasing its references.",
        "#1010 polled release cancellation reports count loss"
    );
    drop(held);
}

#[tokio::test]
async fn lost_release_interrupts_pending_refresh() {
    let (runtime, _) = shutdown_lease_fixture().await;
    let executor = tokio::runtime::Builder::new_current_thread()
        .build()
        .expect("#1010 pending-refresh executor");
    let lease = {
        let _entered = executor.enter();
        runtime
            .acquire_command_catalog()
            .await
            .expect("#1010 pending-refresh lease")
    };
    let (refresh, receiver) = tokio::sync::watch::channel(None);
    runtime.state.lock().await.refresh_receiver = Some(receiver);
    let disposer = runtime.clone();
    let task = tokio::spawn(async move { disposer.dispose().await });
    let completion = tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if let Some(rx) = runtime.state.lock().await.dispose_receiver.clone() {
                break rx;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("#1010 owned disposal starts before lost release");
    assert!(
        completion.borrow().is_none(),
        "#1010 predecessor never completes before loss"
    );
    drop(lease);
    shutdown_executor(executor);
    let error = tokio::time::timeout(Duration::from_secs(3), super::super::wait_watch(completion))
        .await
        .expect("#1010 owned disposal observes lost release boundedly")
        .expect_err("#1010 owned disposal fails on lost release");
    assert_eq!(
        error.message, "Plugin lease release stopped before releasing its references.",
        "#1010 pending refresh does not hide lease failure"
    );
    tokio::time::timeout(Duration::from_secs(3), task)
        .await
        .expect("#1010 pending-refresh public disposal bounded")
        .expect("#1010 pending-refresh public task joined")
        .expect_err("#1010 pending-refresh public failure");
    assert!(
        refresh.borrow().is_none(),
        "#1010 refresh was never falsely completed"
    );
}

#[tokio::test]
async fn lost_release_closes_registry_admission() {
    let (runtime, _) = shutdown_lease_fixture().await;
    let executor = tokio::runtime::Builder::new_current_thread()
        .build()
        .expect("#1010 admission executor");
    let lease = {
        let _entered = executor.enter();
        runtime
            .acquire_command_catalog()
            .await
            .expect("#1010 admission initial lease")
    };
    drop(lease);
    shutdown_executor(executor);
    {
        let _held = runtime.state.lock().await;
        let failure = tokio::time::timeout(Duration::from_secs(3), runtime.refresh())
            .await
            .expect("#1010 lost release rejects refresh before state lock")
            .expect_err("#1010 failed refresh admission without state");
        assert_eq!(
            failure.message, "Plugin lease release stopped before releasing its references.",
            "#1010 lock-free refresh admission preserves failure"
        );
    }
    // No dispose call has transitioned the registry: the loss itself closes it.
    assert!(
        runtime.acquire_command_catalog().await.is_err(),
        "#1010 lost release rejects acquisition"
    );
    assert!(
        runtime.acquire_session_contributions(vec![]).await.is_err(),
        "#1010 lost release rejects session acquisition"
    );
    assert!(
        runtime.acquire_contribution_snapshot().await.is_err(),
        "#1010 lost release rejects legacy acquisition"
    );
    let refreshed = runtime
        .refresh()
        .await
        .expect_err("#1010 lost release rejects refresh");
    assert_eq!(
        refreshed.message, "Plugin lease release stopped before releasing its references.",
        "#1010 lost release rejects refresh with same error"
    );
    assert!(
        runtime
            .state
            .lock()
            .await
            .registry
            .replace_all(vec![])
            .is_err(),
        "#1010 lost release rejects replacement"
    );
}

#[tokio::test]
async fn cancelled_waiter_preserves_last_lease_and_repeated_release() {
    for flavor in ["command", "session", "legacy"] {
        let (runtime, calls) = shutdown_lease_fixture().await;
        let mut first = ShutdownLease::acquire(&runtime, flavor).await;
        let last = runtime
            .acquire_command_catalog()
            .await
            .expect("#1010 last peer lease");
        let disposer = runtime.clone();
        let disposal = tokio::spawn(async move { disposer.dispose().await });
        tokio::time::timeout(Duration::from_secs(3), async {
            while runtime.lifecycle_state().await != super::super::RegistryState::Draining {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("#1010 disposal enters retired lease wait");
        {
            let mut release = Box::pin(first.release());
            assert!(
                release
                    .as_mut()
                    .poll(&mut std::task::Context::from_waker(std::task::Waker::noop()))
                    .is_pending(),
                "#1010 abandoned release waiter initially pending"
            );
        }
        tokio::time::timeout(Duration::from_secs(3), async {
            while shutdown_lease_count(&runtime).await != 1 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("#1010 cancelled waiter still decrements exactly once");
        {
            let mut repeat = Box::pin(first.release());
            assert!(
                repeat
                    .as_mut()
                    .poll(&mut std::task::Context::from_waker(std::task::Waker::noop()))
                    .is_pending(),
                "#1010 repeated release still waits for last lease"
            );
        }
        assert_eq!(
            shutdown_lease_count(&runtime).await,
            1,
            "#1010 repeated release preserves peer reference"
        );
        assert_eq!(
            calls.load(Ordering::SeqCst),
            0,
            "#1010 held peer prevents callback"
        );
        assert!(
            !disposal.is_finished(),
            "#1010 held peer prevents disposal success"
        );
        drop(last);
        tokio::time::timeout(Duration::from_secs(3), disposal)
            .await
            .expect("#1010 final peer release disposal bounded")
            .expect("#1010 final peer disposal joined")
            .expect("#1010 final peer disposal succeeds");
        tokio::time::timeout(Duration::from_secs(3), first.release())
            .await
            .expect("#1010 repeated released waiter bounded")
            .expect("#1010 repeated release succeeds");
        drop(first);
        assert_eq!(
            shutdown_lease_count(&runtime).await,
            0,
            "#1010 live release count zero"
        );
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "#1010 live last release callback exactly once"
        );
        assert!(
            runtime.lease_failure.borrow().is_none(),
            "#1010 healthy release never publishes loss"
        );
    }
}

#[tokio::test]
async fn released_counts_shutdown_does_not_report_lost_lease() {
    let (runtime, _) = shutdown_lease_fixture().await;
    let mut lease = runtime
        .acquire_command_catalog()
        .await
        .expect("#1010 released-count lease");
    let (pending, receiver) = tokio::sync::watch::channel(None);
    runtime
        .state
        .lock()
        .await
        .registry
        .pending_disposals
        .push(super::super::PendingDisposal {
            plugin: lease.plugins[0].clone(),
            receiver,
            sender: None,
        });
    // The actual release has decremented under the lock before it can reach
    // this never-settling callback receiver. Observe that exact Pending poll.
    lease.released = true;
    let guard = LeaseReleaseGuard::new(lease.failure.clone(), true);
    let release =
        super::super::release_lease_plugins(&lease.plugins, &lease.state, &lease.waiters, guard);
    let mut release = Box::pin(release);
    assert!(
        release
            .as_mut()
            .poll(&mut std::task::Context::from_waker(std::task::Waker::noop()))
            .is_pending(),
        "#1010 released-count waits on permanent callback"
    );
    assert_eq!(
        shutdown_lease_count(&runtime).await,
        0,
        "#1010 release decremented before callback wait"
    );
    drop(release); // same cancellation Drop as shutting down the owning executor
    assert!(
        runtime.lease_failure.borrow().is_none(),
        "#1010 released counts disarm loss guard before pending hook"
    );
    assert!(
        pending.borrow().is_none(),
        "#1010 pending callback not falsely settled"
    );
}

#[tokio::test]
async fn empty_lease_dead_executor_is_not_lost() {
    let (runtime, _) = shutdown_lease_fixture().await;
    runtime
        .state
        .lock()
        .await
        .registry
        .replace_all(vec![])
        .expect("#1010 empty registry");
    let executor = tokio::runtime::Builder::new_current_thread()
        .build()
        .expect("#1010 empty executor");
    let lease = {
        let _entered = executor.enter();
        runtime
            .acquire_command_catalog()
            .await
            .expect("#1010 empty lease")
    };
    drop(lease);
    shutdown_executor(executor);
    tokio::time::timeout(Duration::from_secs(3), runtime.dispose())
        .await
        .expect("#1010 empty lease disposal bounded")
        .expect("#1010 empty lease shutdown permits successful disposal");
}

#[tokio::test]
async fn completed_release_result_survives_unrelated_loss() {
    for flavor in ["command", "session", "legacy"] {
        for fails in [false, true] {
            let (runtime, _) = shutdown_lease_fixture().await;
            // Replace with an actual fallible callback before acquiring.
            let plugin = Arc::new(LoadedPlugin {
                name: "completed-1010".into(),
                path: "/virtual/1010/complete".into(),
                source: PluginSource::Builtin,
                commands: vec![],
                skills: vec![],
                output_styles: vec![],
                hooks_config: None,
                mcp_servers: None,
                lsp_servers: None,
                dispose: Some(Box::new(move || {
                    Box::pin(async move {
                        if fails {
                            Err("#1010 original callback failure".into())
                        } else {
                            Ok(())
                        }
                    })
                })),
            });
            runtime
                .state
                .lock()
                .await
                .registry
                .replace_all(vec![plugin])
                .expect("#1010 completed-result generation");
            let mut lease = ShutdownLease::acquire(&runtime, flavor).await;
            // Real retirement starts a callback promise before release.
            let (peer_runtime, _) = shutdown_lease_fixture().await;
            let next = peer_runtime.snapshot().await;
            {
                let mut state = runtime.state.lock().await;
                let retired = state
                    .registry
                    .replace_all(next)
                    .expect("#1010 completed-result replacement")
                    .retired;
                super::super::start_disposals(&mut state.registry, retired, &runtime.state);
            }
            let first = tokio::time::timeout(Duration::from_secs(3), lease.release())
                .await
                .expect("#1010 initial release result bounded");
            assert_eq!(first.is_err(), fails, "#1010 real callback result recorded");
            let executor = tokio::runtime::Builder::new_current_thread()
                .build()
                .expect("#1010 unrelated loss executor");
            let peer = {
                let _entered = executor.enter();
                runtime
                    .acquire_command_catalog()
                    .await
                    .expect("#1010 unrelated peer acquisition")
            };
            drop(peer);
            shutdown_executor(executor);
            let repeated = tokio::time::timeout(Duration::from_secs(3), lease.release())
                .await
                .expect("#1010 completed release repeats boundedly");
            assert_eq!(
                repeated.as_ref().err().map(|e| &e.message),
                first.as_ref().err().map(|e| &e.message),
                "#1010 completed release retains original success or error"
            );
        }
    }
}

#[tokio::test]
async fn refresh_waiting_for_state_observes_lost_release() {
    let (runtime, _) = shutdown_lease_fixture().await;
    let executor = tokio::runtime::Builder::new_current_thread()
        .build()
        .expect("#1010 queued refresh executor");
    let lease = {
        let _entered = executor.enter();
        runtime
            .acquire_command_catalog()
            .await
            .expect("#1010 queued refresh lease")
    };
    let mut held = runtime.state.lock().await;
    let (never, receiver) = tokio::sync::watch::channel(None);
    held.refresh_receiver = Some(receiver);
    let mut refresh = Box::pin(runtime.refresh());
    assert!(
        refresh
            .as_mut()
            .poll(&mut std::task::Context::from_waker(std::task::Waker::noop()))
            .is_pending(),
        "#1010 refresh queued behind held state"
    );
    drop(lease);
    shutdown_executor(executor);
    drop(held);
    let error = tokio::time::timeout(Duration::from_secs(3), refresh)
        .await
        .expect("#1010 queued refresh rechecks loss after lock")
        .expect_err("#1010 queued refresh reports loss");
    assert_eq!(
        error.message, "Plugin lease release stopped before releasing its references.",
        "#1010 queued refresh preserves failure"
    );
    assert!(
        never.borrow().is_none(),
        "#1010 pending refresh was not completed by test"
    );
}

#[tokio::test]
async fn lost_release_interrupts_retired_peer_waiter() {
    let (runtime, _) = shutdown_lease_fixture().await;
    let mut first = runtime
        .acquire_command_catalog()
        .await
        .expect("#1010 retired first lease");
    let executor = tokio::runtime::Builder::new_current_thread()
        .build()
        .expect("#1010 retired peer executor");
    let last = {
        let _entered = executor.enter();
        runtime
            .acquire_command_catalog()
            .await
            .expect("#1010 retired final lease")
    };
    let disposer = runtime.clone();
    let disposal = tokio::spawn(async move { disposer.dispose().await });
    tokio::time::timeout(Duration::from_secs(3), async {
        while runtime.lifecycle_state().await != super::super::RegistryState::Draining {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("#1010 retired disposal waits on leases");
    {
        let mut release = Box::pin(first.release());
        assert!(
            release
                .as_mut()
                .poll(&mut std::task::Context::from_waker(std::task::Waker::noop()))
                .is_pending(),
            "#1010 retired first release pending"
        );
    }
    tokio::time::timeout(Duration::from_secs(3), async {
        while shutdown_lease_count(&runtime).await != 1 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("#1010 retired first release decremented");
    drop(last);
    shutdown_executor(executor);
    let own_release = first
        .release_receiver
        .clone()
        .expect("#1010 retired owned release receiver");
    let error = tokio::time::timeout(
        Duration::from_secs(3),
        super::super::wait_watch(own_release),
    )
    .await
    .expect("#1010 owned release stops waiting on lost peer")
    .expect_err("#1010 retired release error");
    assert_eq!(
        error.message, "Plugin lease release stopped before releasing its references.",
        "#1010 retired peer failure reaches owned release"
    );
    tokio::time::timeout(Duration::from_secs(3), disposal)
        .await
        .expect("#1010 retired disposal stops waiting on loss")
        .expect("#1010 retired disposal joins")
        .expect_err("#1010 retired disposal error");
}
