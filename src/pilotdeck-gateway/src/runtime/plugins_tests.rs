//! Integration tests for PluginRuntime and GatewayCommandCatalogBundle.
//! Real oracle replay is in tests/plugin_loader.rs (A6.3-pr7b scope).

use super::*;

fn lifecycle_oracle() -> serde_json::Value {
    serde_json::from_str::<serde_json::Value>(include_str!(
        "../../tests/fixtures/plugin-loader/oracle.json"
    ))
    .unwrap()["lifecycleCases"]
        .clone()
}

fn runtime_error_oracle(id: &str) -> serde_json::Value {
    let fixture: serde_json::Value = serde_json::from_str(include_str!(
        "../../tests/fixtures/plugin-runtime/oracle.json"
    ))
    .unwrap();
    fixture["scenarios"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["id"] == id)
        .unwrap()["results"]
        .as_array()
        .unwrap()
        .iter()
        .find_map(|r| r.get("error"))
        .unwrap()
        .clone()
}

#[test]
fn registry_duplicate_key_keeps_first_position_and_last_value() {
    // PluginRegistry.ts:73-76,92-93: Map.set replaces a value in place.
    let plugin = |name: &str, path: &str| {
        Arc::new(LoadedPlugin {
            name: name.into(),
            path: path.into(),
            source: PluginSource::Global,
            commands: Vec::new(),
            skills: Vec::new(),
            output_styles: Vec::new(),
            hooks_config: None,
            mcp_servers: None,
            lsp_servers: None,
            dispose: None,
        })
    };
    let first = plugin("z-plugin", "/first");
    let middle = plugin("a-plugin", "/middle");
    let replacement = plugin("z-plugin", "/replacement");
    let mut registry = PluginRegistry::new();

    registry
        .replace_all(vec![first, Arc::clone(&middle), Arc::clone(&replacement)])
        .unwrap();

    let listed = registry.list();
    assert_eq!(listed.len(), 2, "each name@source key must appear once");
    assert!(
        Arc::ptr_eq(&listed[0], &replacement),
        "the first key must keep its position and use the last inserted instance"
    );
    assert!(Arc::ptr_eq(&listed[1], &middle));
}

fn empty_runtime() -> PluginRuntime {
    static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let path = std::env::temp_dir().join(format!(
        "pdrs-missing-runtime-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
    ));
    assert!(!path.exists());
    PluginRuntime::new(PluginRuntimeOptions {
        project_root: path.join("project"),
        pilot_home: path.join("home"),
        builtin_skills_root: None,
        builtin_plugins: Vec::new(),
        builtin_plugins_enabled: HashMap::new(),
    })
}

fn disposable(name: &str, dispose: loader::DisposeFn) -> Arc<LoadedPlugin> {
    Arc::new(LoadedPlugin {
        name: name.into(),
        path: format!("/missing/{name}").into(),
        source: PluginSource::Builtin,
        commands: Vec::new(),
        skills: Vec::new(),
        output_styles: Vec::new(),
        hooks_config: None,
        mcp_servers: None,
        lsp_servers: None,
        dispose: Some(dispose),
    })
}

// Both waiters must be in the same lease-release batch. On the old code the
// release task unwinds after taking the waiters, leaving the second stranded.
#[tokio::test]
async fn disposal_sync_panic_does_not_strand_other_waiters() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;
    for panic_stage in 0..3 {
        let runtime = Arc::new(empty_runtime());
        let completed = Arc::new(AtomicUsize::new(0));
        let completed_cb = Arc::clone(&completed);
        let panic_plugin = disposable(
            "panic",
            Box::new(move || {
                if panic_stage == 0 {
                    panic!("sync dispose failed");
                }
                Box::pin(async move {
                    if panic_stage == 2 {
                        tokio::task::yield_now().await;
                    }
                    panic!("sync dispose failed")
                })
            }),
        );
        let other = disposable(
            "other",
            Box::new(move || {
                completed_cb.fetch_add(1, Ordering::SeqCst);
                Box::pin(async { Ok(()) })
            }),
        );
        runtime
            .state
            .lock()
            .await
            .registry
            .replace_all(vec![panic_plugin, other])
            .unwrap();
        let mut lease = runtime.acquire_command_catalog().await.unwrap();
        runtime.refresh().await.unwrap();
        assert_eq!(
            runtime.state.lock().await.registry.disposal_waiters.len(),
            2
        );
        let shutdown_runtime = Arc::clone(&runtime);
        let shutdown = tokio::spawn(async move { shutdown_runtime.dispose().await });
        tokio::time::timeout(Duration::from_secs(5), async {
            while runtime.lifecycle_state().await != RegistryState::Draining {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        let release = tokio::spawn(async move { lease.release().await });
        // Check shutdown first: the regression must fail at a bounded deadline,
        // even if release itself panics. Dropping the test runtime aborts tasks.
        let error = tokio::time::timeout(Duration::from_secs(5), shutdown)
            .await
            .expect("sync panic stranded a waiter: dispose deadline")
            .unwrap()
            .unwrap_err();
        assert_eq!(completed.load(Ordering::SeqCst), 1);
        let released = tokio::time::timeout(Duration::from_secs(5), release)
            .await
            .unwrap()
            .expect("release must report an error, not panic")
            .unwrap_err();
        assert_eq!(released.message, "sync dispose failed");
        // Error shape comes from a real synchronous-throw callback in TS.
        let expected = runtime_error_oracle("dispose-sync-throw-with-retired-peer");
        fn record(error: &PluginRuntimeError) -> serde_json::Value {
            if error.causes.is_empty() {
                serde_json::json!({"name":"Error", "message":error.message})
            } else {
                serde_json::json!({"name":"AggregateError", "message":error.message,
                    "errors":error.causes.iter().map(record).collect::<Vec<_>>()})
            }
        }
        assert_eq!(record(&error), expected);
        assert_eq!(runtime.lifecycle_state().await, RegistryState::Disposed);
        assert!(
            runtime
                .state
                .lock()
                .await
                .registry
                .pending_disposals
                .is_empty()
        );
    }
}

#[tokio::test]
async fn disposal_prologue_can_access_runtime_state() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;
    for leased in [false, true] {
        let runtime = Arc::new(empty_runtime());
        let observed = Arc::new(AtomicUsize::new(0));
        let state = Arc::clone(&runtime.state);
        let observed_cb = Arc::clone(&observed);
        let plugin = disposable(
            "reentrant",
            Box::new(move || {
                // A current-thread runtime makes try_lock a deterministic assertion
                // about our caller, with no competing state operation in this test.
                assert!(
                    state.try_lock().is_ok(),
                    "DisposeFn invoked under state lock"
                );
                let state = Arc::clone(&state);
                let observed = Arc::clone(&observed_cb);
                Box::pin(async move {
                    assert!(state.try_lock().is_ok(), "callback polled under state lock");
                    let _state = state.lock().await;
                    observed.fetch_add(1, Ordering::SeqCst);
                    Ok(())
                })
            }),
        );
        runtime
            .state
            .lock()
            .await
            .registry
            .replace_all(vec![plugin])
            .unwrap();
        tokio::time::timeout(Duration::from_secs(5), async {
            if leased {
                let mut lease = runtime.acquire_command_catalog().await.unwrap();
                runtime.refresh().await.unwrap();
                lease.release().await.unwrap();
            }
            runtime.dispose().await.unwrap();
        })
        .await
        .expect("reentrant prologue blocked disposal");
        assert_eq!(observed.load(Ordering::SeqCst), 1);
    }
}

#[tokio::test]
async fn disposal_notify_between_first_and_spawned_poll_is_retained() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::mpsc;
    use std::time::Duration;

    for notify_all in [true, false] {
        for leased in [false, true] {
            let notify = Arc::new(Notify::new());
            let entered = Arc::new(AtomicUsize::new(0));
            let completed = Arc::new(AtomicUsize::new(0));
            let notify_cb = Arc::clone(&notify);
            let entered_cb = Arc::clone(&entered);
            let completed_cb = Arc::clone(&completed);
            let plugin = disposable(
                "notify-window",
                Box::new(move || {
                    let notify = Arc::clone(&notify_cb);
                    let entered = Arc::clone(&entered_cb);
                    let completed = Arc::clone(&completed_cb);
                    Box::pin(async move {
                        entered.fetch_add(1, Ordering::SeqCst);
                        notify.notified().await;
                        completed.fetch_add(1, Ordering::SeqCst);
                        Ok(())
                    })
                }),
            );
            let runtime = empty_runtime();
            runtime
                .state
                .lock()
                .await
                .registry
                .replace_all(vec![plugin])
                .unwrap();
            let mut lease = if leased {
                let lease = runtime.acquire_command_catalog().await.unwrap();
                runtime.refresh().await.unwrap();
                Some(lease)
            } else {
                None
            };
            let (first_tx, first_rx) = mpsc::channel();
            let (notified_tx, notified_rx) = mpsc::channel();
            let notifier = std::thread::spawn(move || {
                first_rx.recv_timeout(Duration::from_secs(5)).unwrap();
                if notify_all {
                    notify.notify_waiters();
                } else {
                    notify.notify_one();
                }
                notified_tx.send(()).unwrap();
            });
            let entered_hook = Arc::clone(&entered);
            runtime
                .state
                .lock()
                .await
                .registry
                .after_disposal_first_poll = Some(Box::new(move |pending| {
                assert!(pending, "the callback must register its Notified waiter");
                assert_eq!(entered_hook.load(Ordering::SeqCst), 1);
                // This hook runs before tokio::spawn, so the notification
                // must finish while only the noop waker is registered.
                first_tx.send(()).unwrap();
                notified_rx.recv_timeout(Duration::from_secs(5)).unwrap();
            }));
            let result = tokio::time::timeout(Duration::from_secs(5), async {
                if let Some(lease) = &mut lease {
                    lease.release().await
                } else {
                    runtime.dispose().await
                }
            })
            .await;
            notifier.join().unwrap();
            result
                .expect("disposal lost the notification between first and spawned poll")
                .unwrap();
            assert_eq!(entered.load(Ordering::SeqCst), 1);
            assert_eq!(completed.load(Ordering::SeqCst), 1);
            runtime.dispose().await.unwrap();
            println!(
                "notify_all={notify_all} leased={leased}: first=Pending; notification before spawn; disposal completed"
            );
        }
    }
}

#[tokio::test]
async fn retired_plugin_disposes_once_across_refresh_and_shutdown() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let calls = Arc::new(AtomicUsize::new(0));
    let count = Arc::clone(&calls);
    let plugin = disposable(
        "retired",
        Box::new(move || {
            count.fetch_add(1, Ordering::SeqCst);
            Box::pin(async { Ok(()) })
        }),
    );
    let runtime = empty_runtime();
    runtime
        .state
        .lock()
        .await
        .registry
        .replace_all(vec![plugin])
        .unwrap();
    runtime.refresh().await.unwrap();
    assert_eq!(
        calls.load(Ordering::SeqCst),
        lifecycle_oracle()["sequentialCalls"].as_u64().unwrap() as usize
    );
    assert!(
        runtime
            .state
            .lock()
            .await
            .registry
            .retired_plugins
            .is_empty()
    );
    runtime.dispose().await.unwrap();
    assert_eq!(
        calls.load(Ordering::SeqCst),
        lifecycle_oracle()["sequentialCalls"].as_u64().unwrap() as usize,
        "shutdown must not redispose retired instances"
    );
}

#[tokio::test]
async fn retired_disposal_is_shared_with_shutdown_and_all_lease_flavors() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    // Each flavor must await the callback and forward its failure, rather than
    // merely waking the background retirement and returning Ok.
    for flavor in 0..3 {
        let calls = Arc::new(AtomicUsize::new(0));
        let entered = Arc::new(Notify::new());
        let finish = Arc::new(Notify::new());
        let count = Arc::clone(&calls);
        let entered_cb = Arc::clone(&entered);
        let finish_cb = Arc::clone(&finish);
        let plugin = disposable(
            "failing",
            Box::new(move || {
                count.fetch_add(1, Ordering::SeqCst);
                entered_cb.notify_one();
                let finish = Arc::clone(&finish_cb);
                Box::pin(async move {
                    finish.notified().await;
                    Err("dispose failed".into())
                })
            }),
        );
        let runtime = Arc::new(empty_runtime());
        runtime
            .state
            .lock()
            .await
            .registry
            .replace_all(vec![plugin])
            .unwrap();
        let mut catalog = None;
        let mut session = None;
        let mut contribution = None;
        match flavor {
            0 => catalog = Some(runtime.acquire_command_catalog().await.unwrap()),
            1 => {
                session = Some(
                    runtime
                        .acquire_session_contributions(Vec::new())
                        .await
                        .unwrap(),
                )
            }
            _ => contribution = Some(runtime.acquire_contribution_snapshot().await.unwrap()),
        }
        runtime.refresh().await.unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        let runtime_d = Arc::clone(&runtime);
        let shutdown = tokio::spawn(async move { runtime_d.dispose().await });
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            while runtime.lifecycle_state().await != RegistryState::Draining {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        let release = tokio::spawn(async move {
            let (first, repeated) = match flavor {
                0 => {
                    let lease = catalog.as_mut().unwrap();
                    (lease.release().await, lease.release().await)
                }
                1 => {
                    let lease = session.as_mut().unwrap();
                    (lease.release().await, lease.release().await)
                }
                _ => {
                    let lease = contribution.as_mut().unwrap();
                    (lease.release().await, lease.release().await)
                }
            };
            assert_eq!(
                repeated.as_ref().unwrap_err().message,
                lifecycle_oracle()["repeatReleaseError"].as_str().unwrap()
            );
            assert_eq!(first, repeated, "repeated release must share the result");
            first
        });
        tokio::time::timeout(std::time::Duration::from_secs(5), entered.notified())
            .await
            .unwrap();
        assert!(!release.is_finished(), "release must wait for disposal");
        assert!(
            !shutdown.is_finished(),
            "shutdown must share the pending disposal"
        );
        assert_eq!(
            calls.load(Ordering::SeqCst),
            lifecycle_oracle()["sharedCalls"].as_u64().unwrap() as usize
        );
        finish.notify_one();
        let error = tokio::time::timeout(std::time::Duration::from_secs(5), release)
            .await
            .unwrap()
            .unwrap()
            .unwrap_err();
        assert_eq!(
            error.message,
            lifecycle_oracle()["releaseError"].as_str().unwrap()
        );
        let shutdown_error = shutdown.await.unwrap().unwrap_err();
        assert_eq!(
            shutdown_error.message,
            lifecycle_oracle()["shutdownError"].as_str().unwrap()
        );
        assert_eq!(
            shutdown_error.causes.len(),
            runtime_error_oracle("dispose-single-error")["errors"]
                .as_array()
                .unwrap()
                .len()
        );
        assert!(
            shutdown_error
                .causes
                .iter()
                .all(|e| e.message == "dispose failed")
        );
        assert_eq!(
            calls.load(Ordering::SeqCst),
            lifecycle_oracle()["sharedCalls"].as_u64().unwrap() as usize
        );
        assert_eq!(
            runtime.snapshot().await.len(),
            lifecycle_oracle()["finalCount"].as_u64().unwrap() as usize
        );
    }
}

#[tokio::test]
async fn dispose_waits_for_refresh_publication_then_drains_its_generation() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let runtime = Arc::new(empty_runtime());
    // A watch barrier fixes the interval between disk staging and publication;
    // no timing assumption about the host's filesystem is involved.
    let (tx, rx) = watch::channel(None);
    runtime.state.lock().await.refresh_receiver = Some(rx);
    let runtime_d = Arc::clone(&runtime);
    let shutdown = tokio::spawn(async move { runtime_d.dispose().await });
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while !runtime.state.lock().await.disposal_requested {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(
        runtime.lifecycle_state().await.as_str(),
        lifecycle_oracle()["stateWhileRefresh"].as_str().unwrap()
    );
    assert_eq!(
        runtime.refresh().await.unwrap_err().message,
        runtime_error_oracle("dispose-waits-for-pending-refresh")["message"]
            .as_str()
            .unwrap()
    );
    let calls = Arc::new(AtomicUsize::new(0));
    let count = Arc::clone(&calls);
    let plugin = disposable(
        "new-generation",
        Box::new(move || {
            count.fetch_add(1, Ordering::SeqCst);
            Box::pin(async { Ok(()) })
        }),
    );
    let replacement = runtime
        .state
        .lock()
        .await
        .registry
        .replace_all(vec![plugin])
        .unwrap();
    tx.send(Some(Ok(PluginRefreshResult {
        generation: replacement.generation,
        previous: replacement.previous,
        next: replacement.next,
        added: Vec::new(),
        removed: Vec::new(),
        staged_failures: Vec::new(),
    })))
    .unwrap();
    let other_runtime = Arc::clone(&runtime);
    let second_shutdown = tokio::spawn(async move { other_runtime.dispose().await });
    tokio::time::timeout(std::time::Duration::from_secs(5), shutdown)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    second_shutdown.await.unwrap().unwrap();
    assert_eq!(
        calls.load(Ordering::SeqCst),
        lifecycle_oracle()["newCalls"].as_u64().unwrap() as usize
    );
    assert_eq!(
        runtime.snapshot().await.len(),
        lifecycle_oracle()["finalCount"].as_u64().unwrap() as usize
    );
    assert_eq!(runtime.lifecycle_state().await, RegistryState::Disposed);
    let mut st = runtime.state.lock().await;
    let generation = st.registry.generation;
    assert_eq!(
        st.registry.replace_all(Vec::new()).err().unwrap().message,
        lifecycle_oracle()["publicationError"].as_str().unwrap()
    );
    assert_eq!(st.registry.generation, generation);
}

#[test]
fn registry_rejects_publication_while_draining() {
    let mut registry = PluginRegistry::new();
    registry.state = RegistryState::Draining;
    assert_eq!(
        registry.replace_all(Vec::new()).err().unwrap().message,
        "Cannot replace plugins; plugin registry is draining."
    );
    assert_eq!(registry.generation, 0);
}

#[tokio::test]
async fn skill_contributions_keep_ts_map_order() {
    let fixture: serde_json::Value = serde_json::from_str(include_str!(
        "../../tests/fixtures/plugin-loader/regressions.json"
    ))
    .unwrap();
    let fixture = &fixture["skills"];
    let runtime = empty_runtime();
    let plugins = fixture["specs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|spec| {
            let mut plugin = disposable("same", Box::new(|| Box::pin(async { Ok(()) })));
            let p = Arc::get_mut(&mut plugin).unwrap();
            p.source = match spec["source"].as_str().unwrap() {
                "global" => PluginSource::Global,
                "builtin" => PluginSource::Builtin,
                "project" => PluginSource::Project,
                _ => unreachable!(),
            };
            p.skills = spec["values"]
                .as_array()
                .unwrap()
                .iter()
                .map(|v| loader::LoadedPluginCommand {
                    name: v[0].as_str().unwrap().into(),
                    path: format!("/skills/{}/SKILL.md", v[0].as_str().unwrap()).into(),
                    content: v[1].as_str().unwrap().into(),
                    frontmatter: HashMap::from([("description".into(), v[1].clone())]),
                    is_skill: true,
                })
                .collect();
            plugin
        })
        .collect();
    runtime
        .state
        .lock()
        .await
        .registry
        .replace_all(plugins)
        .unwrap();
    let encode = |skills: &[PluginSkillContribution]| {
        serde_json::Value::Array(skills.iter().map(|s| serde_json::json!({"name":s.name,"description":s.description,"content":s.content,"namespace":s.namespace})).collect())
    };
    assert_eq!(encode(&runtime.all_skills().await), fixture["result"]);
    let mut session = runtime
        .acquire_session_contributions(Vec::new())
        .await
        .unwrap();
    let mut legacy = runtime.acquire_contribution_snapshot().await.unwrap();
    assert_eq!(encode(&session.contributions.skills), fixture["result"]);
    assert_eq!(encode(&legacy.contributions.skills), fixture["result"]);
    session.release().await.unwrap();
    legacy.release().await.unwrap();
    runtime.dispose().await.unwrap();
}
