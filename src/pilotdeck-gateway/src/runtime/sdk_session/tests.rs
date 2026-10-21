//! Unit tests for SDK session configuration. Oracle replay lives in
//! `tests/sdk_session.rs`.
//!
//! Every test that touches the filesystem owns one [`scratch::Scratch`] (UUID
//! name, owner token, drop refuses a replaced path). No fixed temporary path
//! is read or removed.

use crate::test_scratch as scratch;

use super::*;
use crate::{
    config::{Env, session_overrides::SessionConfigOverrides, session_value::SessionValue},
    runtime::plugins::{LoadedPlugin, PluginRuntime, PluginRuntimeOptions, PluginSource},
    storage::{TaskBudgetError, TaskBudgetStore},
};
use std::{
    collections::HashMap,
    ffi::OsString,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

struct FixedResolver {
    root: PathBuf,
    plugin_runtime: Arc<PluginRuntime>,
}

impl SdkRuntimeResolver for FixedResolver {
    fn resolve(
        &self,
        _project_key: Option<&JsString>,
    ) -> Result<ResolvedSdkRuntime, SdkSessionError> {
        Ok(ResolvedSdkRuntime {
            project_root: self.root.clone(),
            snapshot: snapshot(),
            plugin_runtime: Arc::clone(&self.plugin_runtime),
        })
    }
}

struct NamedLoader;

impl SdkPluginLoader for NamedLoader {
    fn load<'a>(&'a self, path: &'a Path) -> PluginLoadFuture<'a> {
        let name = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        let path = path.to_path_buf();
        Box::pin(async move {
            Ok(LoadedPlugin {
                name,
                path,
                source: PluginSource::Project,
                commands: Vec::new(),
                skills: Vec::new(),
                output_styles: Vec::new(),
                hooks_config: None,
                mcp_servers: None,
                lsp_servers: None,
                dispose: None,
            })
        })
    }
}

fn snapshot() -> JsValue {
    js_object(vec![(
        "config",
        js_object(vec![
            (
                "agent",
                js_object(vec![(
                    "model",
                    js_object(vec![
                        ("id", "main".into()),
                        ("provider", "p1".into()),
                        ("model", "m1".into()),
                    ]),
                )]),
            ),
            (
                "model",
                js_object(vec![(
                    "providers",
                    js_object(vec![
                        (
                            "p1",
                            js_object(vec![(
                                "models",
                                js_object(vec![("m1", js_object(vec![("id", "m1".into())]))]),
                            )]),
                        ),
                        (
                            "p2",
                            js_object(vec![(
                                "models",
                                js_object(vec![("m1", js_object(vec![("id", "m1".into())]))]),
                            )]),
                        ),
                    ]),
                )]),
            ),
        ]),
    )])
}

fn registry(root: &Path, policy: Option<OrganizationPolicy>) -> SdkSessionRegistry {
    registry_with_overrides(root, policy, SessionConfigOverrides::new())
}

fn registry_with_overrides(
    root: &Path,
    policy: Option<OrganizationPolicy>,
    overrides: SessionConfigOverrides,
) -> SdkSessionRegistry {
    let runtime = Arc::new(PluginRuntime::new(PluginRuntimeOptions {
        project_root: root.to_path_buf(),
        pilot_home: root.join("home"),
        builtin_skills_root: None,
        builtin_plugins: Vec::new(),
        builtin_plugins_enabled: HashMap::new(),
    }));
    let mut env = Env::new();
    env.insert("HOME".into(), root.to_string_lossy().into_owned());
    env.insert(
        "PILOT_HOME".into(),
        root.join("home").to_string_lossy().into_owned(),
    );
    SdkSessionRegistry::new(
        SdkSessionOptions::new(
            root.join("fallback"),
            root,
            env,
            Arc::new(FixedResolver {
                root: root.to_path_buf(),
                plugin_runtime: runtime,
            }),
        )
        .with_homedir(root)
        .with_organization_policy(policy)
        .with_plugin_loader(Arc::new(NamedLoader))
        .with_clock(Arc::new(|| 1_000.0)),
    )
    .with_overrides(overrides)
}

#[test]
fn settings_errors_keep_the_baseline_name_until_the_registry_maps_them() {
    let empty = JsValue::Array(Vec::new());
    let error = validate_setting_sources(Some(&empty)).unwrap_err();
    assert_eq!(error.name, "PilotSdkSessionSettingsError");
    assert_eq!(error.code, "INVALID_SDK_SETTING_SOURCES");

    let error = validate_session_settings(Some(&js_object(vec![("foo", JsValue::Number(1.0))])))
        .unwrap_err();
    assert_eq!(error.name, "PilotSdkSessionSettingsError");
    assert_eq!(error.code, "UNSUPPORTED_SDK_SETTING");
    assert!(error.message.contains("only non-secret agent model"));
}

#[tokio::test]
async fn set_config_maps_settings_errors_and_leaves_state_unchanged() {
    let scratch = scratch::Scratch::new("sdk-session-map");
    let mut registry = registry(scratch.path(), None);
    let config = js_object(vec![("settingSources", JsValue::Array(Vec::new()))]);
    let error = registry
        .set_session_config(&"s".into(), &config, None, None)
        .await
        .unwrap_err();
    assert_eq!(error.name, "DialogGatewayError");
    assert_eq!(error.code, "INVALID_SDK_SETTING_SOURCES");
    assert!(registry.config_entries().is_empty());
    assert!(registry.project_entries().is_empty());
}

#[test]
fn task_budget_store_port_reuses_the_merged_store() {
    let scratch = scratch::Scratch::new("sdk-session-budget");
    let store = TaskBudgetStore::new(scratch.path(), None);
    let ledger_path = store.path().to_path_buf();
    let port = TaskBudgetStorePort::new(store);
    let root: JsString = "project".into();
    let session: JsString = "s".into();
    port.configure_project(&root, &session, 5.0, Some(1_000.0), 10.0)
        .unwrap();
    port.configure_project(&root, &session, 5.0, Some(1_000.0), 20.0)
        .unwrap();
    let conflict = port
        .configure_project(&root, &session, 6.0, Some(1_000.0), 30.0)
        .unwrap_err();
    assert_eq!(
        conflict.code(),
        Some("SDK_PROJECT_TASK_BUDGET_TOTAL_CONFLICT")
    );
    let retention = port
        .configure_project(&root, &session, 5.0, Some(2_000.0), 30.0)
        .unwrap_err();
    assert_eq!(
        retention.code(),
        Some("SDK_PROJECT_TASK_BUDGET_RETENTION_CONFLICT")
    );
    port.clear_session(&root, &session).unwrap();
    let text = std::fs::read_to_string(&ledger_path).unwrap();
    assert_eq!(
        text.lines().count(),
        2,
        "one configured and one cleared record"
    );
    assert!(
        text.lines()
            .next()
            .unwrap()
            .contains("\"kind\":\"configured\"")
    );
    assert!(
        text.lines()
            .nth(1)
            .unwrap()
            .contains("\"kind\":\"cleared\"")
    );
}

#[tokio::test]
async fn plugin_lifecycle_and_clear_state() {
    let scratch = scratch::Scratch::new("sdk-session-plugins");
    let plugin_dir = scratch.path().join("plugin-a");
    std::fs::create_dir_all(&plugin_dir).unwrap();
    let mut registry = registry(scratch.path(), None);
    let config = js_object(vec![(
        "plugins",
        JsValue::Array(vec![js_object(vec![(
            "path",
            JsValue::String(plugin_dir.to_string_lossy().into_owned().into()),
        )])]),
    )]);
    assert!(
        registry
            .set_session_config(&"s".into(), &config, None, None)
            .await
            .unwrap()
    );
    assert_eq!(registry.session_plugins(&"s".into()).len(), 1);
    assert_eq!(registry.session_plugins(&"s".into())[0].name, "plugin-a");
    assert_eq!(registry.session_project(&"s".into()), Some(scratch.path()));
    assert!(
        !registry
            .set_session_config(&"s".into(), &config, None, None)
            .await
            .unwrap(),
        "identical configuration is unchanged"
    );
    registry
        .set_session_config(
            &"s".into(),
            &js_object(Vec::<(&str, JsValue)>::new()),
            None,
            None,
        )
        .await
        .unwrap();
    assert!(registry.session_plugins(&"s".into()).is_empty());
    registry.set_session_thinking(&"s".into(), Some(&JsValue::Null));
    registry.register_config_change_handler(&"s".into(), Arc::new(|_| {}));
    assert!(registry.handlers().has_config_change(&"s".into()));
    registry.clear_session_state(&"s".into()).unwrap();
    assert!(registry.session_config(&"s".into()).is_none());
    assert!(registry.session_project(&"s".into()).is_none());
    assert!(registry.thinking_override(&"s".into()).is_none());
    assert!(!registry.handlers().has_config_change(&"s".into()));
}

#[test]
fn flag_settings_tristate_and_existing_override_removal() {
    let scratch = scratch::Scratch::new("sdk-session-flags");
    let mut registry = registry(scratch.path(), None);
    let key: JsString = "s".into();

    // Missing key: no-op.
    let result = registry
        .apply_flag_settings(&key, &js_object(Vec::<(&str, JsValue)>::new()))
        .unwrap();
    assert!(result.applied.is_empty() && result.cleared.is_empty());

    // Own property with undefined sets `mode: undefined`, not a delete.
    let result = registry
        .apply_flag_settings(&key, &js_object(vec![("effortLevel", JsValue::Undefined)]))
        .unwrap();
    assert_eq!(result.applied, vec!["effortLevel"]);
    let thinking = registry.thinking_override(&key).unwrap();
    assert_eq!(thinking.get("enabled"), Some(&JsValue::Bool(true)));
    assert!(thinking.get("mode").unwrap().is_undefined());

    // null deletes.
    let result = registry
        .apply_flag_settings(&key, &js_object(vec![("effortLevel", JsValue::Null)]))
        .unwrap();
    assert_eq!(result.cleared, vec!["effortLevel"]);
    assert!(registry.thinking_override(&key).is_none());

    // permissions: undefined is a raw TypeError, matching the baseline.
    let error = registry
        .apply_flag_settings(&key, &js_object(vec![("permissions", JsValue::Undefined)]))
        .unwrap_err();
    assert_eq!(error.name, "TypeError");
    assert_eq!(
        error.message,
        "Cannot read properties of undefined (reading 'defaultMode')"
    );

    // null clears a previously stored permissionMode.
    let result = registry
        .apply_flag_settings(
            &key,
            &js_object(vec![(
                "permissions",
                js_object(vec![("defaultMode", "plan".into())]),
            )]),
        )
        .unwrap();
    assert_eq!(result.applied, vec!["permissions.defaultMode"]);
    let result = registry
        .apply_flag_settings(&key, &js_object(vec![("permissions", JsValue::Null)]))
        .unwrap();
    assert_eq!(result.cleared, vec!["permissions.defaultMode"]);
    let overrides = registry.session_overrides().unwrap();
    assert_eq!(
        overrides.get(&key).unwrap().to_value(),
        JsValue::Object(JsObject::new())
    );
}

#[test]
fn flag_settings_preserve_nested_override_handles() {
    let scratch = scratch::Scratch::new("sdk-session-flag-identity");
    let key: JsString = "s".into();
    // Seed the store the way A6.6/A6.7 will: one permissionRules handle that
    // outlives the flag writes. TS spreads the top level in both branches, so
    // the handle must still compare equal afterwards.
    let rules = SessionValue::object();
    rules.set("allow", SessionValue::array(vec![]));
    let held = SessionValue::object();
    held.set("permissionRules", rules);
    let mut overrides = SessionConfigOverrides::new();
    overrides.set(&key, &held);
    let mut registry = registry_with_overrides(scratch.path(), None, overrides);
    assert!(
        registry
            .session_overrides()
            .unwrap()
            .same_permission_rules(&key, &held)
    );

    registry
        .apply_flag_settings(
            &key,
            &js_object(vec![(
                "permissions",
                js_object(vec![("defaultMode", "plan".into())]),
            )]),
        )
        .unwrap();
    assert!(
        registry
            .session_overrides()
            .unwrap()
            .same_permission_rules(&key, &held),
        "applying permissionMode must keep the nested permissionRules handle"
    );

    registry
        .apply_flag_settings(&key, &js_object(vec![("permissions", JsValue::Null)]))
        .unwrap();
    assert!(
        registry
            .session_overrides()
            .unwrap()
            .same_permission_rules(&key, &held),
        "clearing permissionMode must keep the nested permissionRules handle"
    );
}

#[test]
fn handler_registration_replaces_and_stale_tokens_are_noops() {
    let scratch = scratch::Scratch::new("sdk-session-handlers");
    let mut registry = registry(scratch.path(), None);
    let key: JsString = "s".into();
    let first = registry.register_config_change_handler(&key, Arc::new(|_| {}));
    let second = registry.register_config_change_handler(&key, Arc::new(|_| {}));
    assert!(registry.handlers().has_config_change(&key));
    registry.handlers_mut().unregister_config_change(&first);
    assert!(
        registry.handlers().has_config_change(&key),
        "stale token must not remove the newer registration"
    );
    registry.handlers_mut().unregister_config_change(&second);
    assert!(!registry.handlers().has_config_change(&key));
}

#[test]
fn task_budget_cap_uses_js_math_min_and_preserves_optional_fields() {
    let mut config = js_object(vec![
        (
            "taskBudget",
            js_object(vec![("total", JsValue::Number(9.0))]),
        ),
        ("keep", JsValue::Number(1.0)),
    ]);
    apply_organization_task_budget_cap(&mut config, Some(3.0));
    assert_eq!(
        config.get("taskBudget").unwrap(),
        &js_object(vec![("total", JsValue::Number(3.0))])
    );
    assert_eq!(config.get("keep"), Some(&JsValue::Number(1.0)));

    let mut config = js_object(vec![(
        "taskBudget",
        js_object(vec![
            ("total", JsValue::Number(9.0)),
            ("scope", "project".into()),
            ("projectRetentionMs", JsValue::Number(100.0)),
        ]),
    )]);
    apply_organization_task_budget_cap(&mut config, Some(3.0));
    assert_eq!(
        config.get("taskBudget").unwrap(),
        &js_object(vec![
            ("total", JsValue::Number(3.0)),
            ("scope", "project".into()),
            ("projectRetentionMs", JsValue::Number(100.0)),
        ])
    );

    let mut config = js_object(vec![(
        "taskBudget",
        js_object(vec![("total", JsValue::Number(f64::NAN))]),
    )]);
    apply_organization_task_budget_cap(&mut config, Some(3.0));
    assert!(
        config
            .get("taskBudget")
            .unwrap()
            .get("total")
            .unwrap()
            .as_number()
            .unwrap()
            .is_nan()
    );
}

#[test]
fn selector_star_and_trimmed_duplicates_follow_the_regex() {
    let star = js_object(vec![
        ("allow", JsValue::Array(vec!["*".into()])),
        ("deny", JsValue::Array(Vec::new())),
    ]);
    validate_selector_policy(Some(&star), SelectorKind::Tools).unwrap();
    validate_selector_policy(Some(&star), SelectorKind::Models).unwrap();

    let duplicate = js_object(vec![
        ("allow", JsValue::Array(vec!["a".into(), " a ".into()])),
        ("deny", JsValue::Array(Vec::new())),
    ]);
    let error = validate_selector_policy(Some(&duplicate), SelectorKind::Tools).unwrap_err();
    assert_eq!(error.code, "INVALID_SDK_MANAGED_SETTINGS");
    assert!(error.message.contains("cannot contain duplicates"));
}

#[tokio::test]
async fn set_config_clears_recorded_project_budget() {
    let scratch = scratch::Scratch::new("sdk-session-clear-budget");
    let cleared = Arc::new(Mutex::new(Vec::new()));
    struct Budget {
        cleared: Arc<Mutex<Vec<JsString>>>,
    }
    impl SdkTaskBudget for Budget {
        fn configure_project(
            &self,
            _project_root: &JsString,
            _session_key: &JsString,
            _total_usd: f64,
            _retention_ms: Option<f64>,
            _now_ms: f64,
        ) -> Result<(), TaskBudgetError> {
            Ok(())
        }
        fn clear_session(
            &self,
            project_root: &JsString,
            _session_key: &JsString,
        ) -> Result<(), TaskBudgetError> {
            self.cleared.lock().unwrap().push(project_root.clone());
            Ok(())
        }
    }
    let runtime = Arc::new(PluginRuntime::new(PluginRuntimeOptions {
        project_root: scratch.path().to_path_buf(),
        pilot_home: scratch.path().join("home"),
        builtin_skills_root: None,
        builtin_plugins: Vec::new(),
        builtin_plugins_enabled: HashMap::new(),
    }));
    let mut env = Env::new();
    env.insert("HOME".into(), scratch.path().to_string_lossy().into_owned());
    let mut registry = SdkSessionRegistry::new(
        SdkSessionOptions::new(
            scratch.path(),
            scratch.path(),
            env,
            Arc::new(FixedResolver {
                root: scratch.path().to_path_buf(),
                plugin_runtime: runtime,
            }),
        )
        .with_homedir(scratch.path())
        .with_plugin_loader(Arc::new(NamedLoader))
        .with_task_budget(Arc::new(Budget {
            cleared: Arc::clone(&cleared),
        })),
    );
    let config = js_object(vec![(
        "taskBudget",
        js_object(vec![
            ("total", JsValue::Number(1.0)),
            ("scope", "project".into()),
        ]),
    )]);
    registry
        .set_session_config(&"s".into(), &config, None, None)
        .await
        .unwrap();
    registry.clear_session_state(&"s".into()).unwrap();
    assert_eq!(cleared.lock().unwrap().len(), 1);
}

#[test]
fn abort_signal_is_checked_before_state_changes() {
    let scratch = scratch::Scratch::new("sdk-session-abort");
    let mut registry = registry(scratch.path(), None);
    struct Aborted;
    impl SdkAbortSignal for Aborted {
        fn throw_if_aborted(&self) -> Result<(), SdkSessionError> {
            Err(SdkSessionError::raw("AbortError", "stopped"))
        }
    }
    let error = futures_block_on(registry.set_session_config(
        &"s".into(),
        &js_object(Vec::<(&str, JsValue)>::new()),
        None,
        Some(&Aborted),
    ))
    .unwrap_err();
    assert_eq!(error.name, "AbortError");
    assert!(registry.config_entries().is_empty());
}

#[test]
fn hook_event_enum_matches_shared_protocol_list() {
    let shared = crate::hooks::protocol::HOOK_EVENTS;
    assert_eq!(PilotDeckHookEvent::ALL.len(), shared.len());
    for (index, event) in PilotDeckHookEvent::ALL.iter().enumerate() {
        assert_eq!(event.as_str(), shared[index], "hook event {index}");
    }
}

#[test]
fn home_dir_falls_back_to_process_home() {
    // Gate1 B6 wiring: without a host seam `home_dir` resolves the process
    // HOME snapshot through `home_dir_from` (which handles the unset case).
    assert_eq!(home_dir(None), home_dir_from(std::env::var_os("HOME")));
}

#[test]
fn home_dir_without_home_uses_the_platform_home() {
    // With HOME treated as unset, `std::env::home_dir()` reads the passwd entry;
    // on this machine that is a real absolute directory, not the historical `/`.
    // The exact value cannot be built directly in the test, so assert the two
    // properties that separate the passwd fallback from that hard-coded default.
    let home = home_dir_from(None);
    assert_ne!(home, PathBuf::from("/"));
    assert!(home.is_absolute());
}

#[test]
fn home_dir_keeps_empty_home_value() {
    // libuv returns an empty HOME as-is (it only consults passwd when HOME is
    // unset); it must not become `/`.
    assert_eq!(home_dir_from(Some(OsString::new())), PathBuf::from(""));
}

#[test]
fn injected_env_home_does_not_shadow_os_homedir() {
    let scratch = scratch::Scratch::new("sdk-session-home-source");
    let process_home = scratch.path().join("process-home");
    let env_home = scratch.path().join("env-home");
    for (home, model) in [
        (&process_home, "process-home-model"),
        (&env_home, "env-home-model"),
    ] {
        let directory = home.join("settings");
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::write(
            directory.join("pilotdeck.yaml"),
            format!("agent: {{model: {model}}}\n"),
        )
        .unwrap();
    }

    let mut env = Env::new();
    env.insert("HOME".into(), env_home.to_string_lossy().into_owned());
    env.insert("PILOT_HOME".into(), "~/settings".into());
    let sources = JsValue::Array(vec!["user".into()]);
    let resolved = resolve_session_settings(ResolveSettingsOptions {
        project_root: scratch.path(),
        cwd: scratch.path(),
        env: &env,
        // Stands in for `os.homedir()`; tests cannot rewrite the process HOME.
        homedir: Some(&process_home),
        host_defaults: None,
        host_managed_settings: None,
        host_setting_sources: Some(&sources),
        settings: None,
        setting_sources: None,
        host_enforced_settings: None,
    })
    .unwrap()
    .expect("process-home settings resolve");

    let model = resolved
        .get("agent")
        .and_then(|agent| agent.get("model"))
        .and_then(JsValue::as_string)
        .map(|model| model.to_utf8().unwrap());
    assert_eq!(model.as_deref(), Some("process-home-model"));
}

#[test]
fn read_failure_message_uses_node_errno_text() {
    let path = Path::new("/tmp/settings.yaml");
    // ENOENT is unreachable through TS (`existsSync` returns false first), so
    // pin its Node text at the mapping boundary instead of the file oracle.
    for (errno, expected) in [
        (
            libc::ENOENT,
            "ENOENT: no such file or directory, open '/tmp/settings.yaml'",
        ),
        (
            libc::EACCES,
            "EACCES: permission denied, open '/tmp/settings.yaml'",
        ),
        (
            libc::EISDIR,
            "EISDIR: illegal operation on a directory, read",
        ),
    ] {
        assert_eq!(
            read_failure_message(&std::io::Error::from_raw_os_error(errno), path),
            expected
        );
    }
}

#[tokio::test]
async fn abort_after_project_budget_write_keeps_ledger_and_retry_conflicts() {
    struct FourthAbort(std::cell::Cell<usize>);
    impl SdkAbortSignal for FourthAbort {
        fn throw_if_aborted(&self) -> Result<(), SdkSessionError> {
            let calls = self.0.get() + 1;
            self.0.set(calls);
            if calls >= 4 {
                return Err(SdkSessionError::raw("Error", "aborted-fourth"));
            }
            Ok(())
        }
    }

    let scratch = scratch::Scratch::new("sdk-session-budget-abort");
    let home = scratch.path().join("home");
    let store = TaskBudgetStore::new(&home, None);
    let ledger_path = store.path().to_path_buf();
    let runtime = Arc::new(PluginRuntime::new(PluginRuntimeOptions {
        project_root: scratch.path().to_path_buf(),
        pilot_home: home.clone(),
        builtin_skills_root: None,
        builtin_plugins: Vec::new(),
        builtin_plugins_enabled: HashMap::new(),
    }));
    let mut env = Env::new();
    env.insert("HOME".into(), scratch.path().to_string_lossy().into_owned());
    env.insert("PILOT_HOME".into(), home.to_string_lossy().into_owned());
    let mut registry = SdkSessionRegistry::new(
        SdkSessionOptions::new(
            scratch.path(),
            scratch.path(),
            env,
            Arc::new(FixedResolver {
                root: scratch.path().to_path_buf(),
                plugin_runtime: runtime,
            }),
        )
        .with_homedir(scratch.path())
        .with_plugin_loader(Arc::new(NamedLoader))
        .with_task_budget(Arc::new(TaskBudgetStorePort::new(store)))
        .with_clock(Arc::new(|| 0.0_f64)),
    )
    .with_overrides(SessionConfigOverrides::new());

    let budget = |total: f64, retention: Option<f64>| {
        let mut entries = vec![
            ("total", JsValue::Number(total)),
            ("scope", JsValue::String("project".into())),
        ];
        if let Some(retention) = retention {
            entries.push(("projectRetentionMs", JsValue::Number(retention)));
        }
        js_object(vec![("taskBudget", js_object(entries))])
    };
    let error = registry
        .set_session_config(
            &"s".into(),
            &budget(5.0, Some(100.0)),
            None,
            Some(&FourthAbort(std::cell::Cell::new(0))),
        )
        .await
        .unwrap_err();
    assert_eq!(error.name, "Error");
    assert_eq!(error.message, "aborted-fourth");
    assert!(
        registry.session_config(&"s".into()).is_none(),
        "abort after the ledger write must not store the session config"
    );
    assert!(registry.project_entries().is_empty());
    let ledger = std::fs::read_to_string(&ledger_path).unwrap();
    let lines: Vec<_> = ledger.lines().collect();
    assert_eq!(lines.len(), 1, "the configured record is on disk");
    assert!(lines[0].contains("\"kind\":\"configured\""));
    assert!(lines[0].contains("\"totalUsd\":5"));

    let error = registry
        .set_session_config(&"s".into(), &budget(6.0, None), None, None)
        .await
        .unwrap_err();
    assert_eq!(
        error.code(),
        "SDK_PROJECT_TASK_BUDGET_TOTAL_CONFLICT",
        "the ledger survives the aborted config"
    );
    assert!(error.message.contains("received $6.000000"));
    assert!(registry.session_config(&"s".into()).is_none());
}

/// Minimal block-on for the one synchronous test that needs an async method.
fn futures_block_on<F: std::future::Future>(future: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(future)
}
