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
    assert_eq!(calls.load(Ordering::SeqCst), 1);
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
        1,
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
        assert_eq!(calls.load(Ordering::SeqCst), 1);
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
        assert_eq!(shutdown_error.causes.len(), 2);
        assert!(
            shutdown_error
                .causes
                .iter()
                .all(|e| e.message == "dispose failed")
        );
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert!(runtime.snapshot().await.is_empty());
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
    assert!(
        runtime
            .refresh()
            .await
            .unwrap_err()
            .message
            .contains("draining")
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
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert!(runtime.snapshot().await.is_empty());
    assert_eq!(runtime.lifecycle_state().await, RegistryState::Disposed);
    let mut st = runtime.state.lock().await;
    let generation = st.registry.generation;
    assert_eq!(
        st.registry.replace_all(Vec::new()).err().unwrap().message,
        "Cannot replace plugins; plugin registry is disposed."
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
