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
