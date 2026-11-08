use super::*;
use crate::{
    config::Env,
    runtime::sdk_session::{ResolveSettingsOptions, resolve_session_settings_with_reader},
};
use pilotdeck_protocol::JsValue;
#[cfg(unix)]
#[test]
fn setting_sources_bound_real_fifo_sparse_and_directory_without_changing_normal_files() {
    let scratch = crate::test_scratch::Scratch::new("sdk-source-bound");
    let home = scratch.path().join("home");
    std::fs::create_dir(&home).unwrap();
    let file = home.join("pilotdeck.yaml");
    let normal = b"agent:\n  maxOutputTokens: 42\n";
    std::fs::write(&file, normal).unwrap();
    let child = "native_process::assembly::sdk_settings::tests::setting_source_child";
    for kind in ["normal", "fifo", "large", "directory"] {
        match kind {
            "normal" => {}
            "fifo" => {
                std::fs::remove_file(&file).unwrap();
                assert!(
                    std::process::Command::new("/usr/bin/mkfifo")
                        .args(["-m", "600"])
                        .arg(&file)
                        .status()
                        .unwrap()
                        .success()
                );
            }
            "large" => {
                std::fs::remove_file(&file).unwrap();
                std::fs::File::create(&file)
                    .unwrap()
                    .set_len(8 * 1024 * 1024 + 1)
                    .unwrap();
            }
            "directory" => {
                std::fs::remove_file(&file).unwrap();
                std::fs::create_dir(&file).unwrap();
            }
            _ => unreachable!(),
        }
        let root = home.to_str().unwrap();
        let output = crate::test_process::rerun_exact(
            child,
            &[
                ("PDRS_SDK_SOURCE_HOME", root),
                ("PDRS_SDK_SOURCE_KIND", kind),
            ],
            std::time::Duration::from_secs(3),
        );
        assert!(
            !output.timed_out && !output.overflow,
            "{kind}: bounded source child timed out: {} {}",
            output.stdout,
            output.stderr
        );
        assert!(
            crate::test_exact_receipt::single_pass(&output.status, child, &output.stdout),
            "{kind}: {} {}",
            output.stdout,
            output.stderr
        );
        let expected = format!("PDRS_CASE_OK {kind}");
        assert_eq!(
            output
                .stdout
                .lines()
                .filter(|l| l.starts_with("PDRS_CASE_OK "))
                .collect::<Vec<_>>(),
            vec![expected.as_str()]
        );
        if kind == "normal" {
            assert_eq!(std::fs::read(&file).unwrap(), normal);
        } else if kind == "large" {
            assert_eq!(std::fs::metadata(&file).unwrap().len(), 8 * 1024 * 1024 + 1);
        }
    }
}
#[cfg(unix)]
#[test]
#[ignore = "owned setting-source worker, invoked by parent"]
fn setting_source_child() {
    let home =
        std::path::PathBuf::from(std::env::var_os("PDRS_SDK_SOURCE_HOME").expect("owned home"));
    let kind = std::env::var("PDRS_SDK_SOURCE_KIND").unwrap();
    let env = source_environment(&Env::new(), &home);
    let sources = JsValue::Array(vec!["user".into()]);
    let wrong = home.join("must-not-read-os-home");
    let result = resolve_session_settings_with_reader(
        ResolveSettingsOptions {
            project_root: &home,
            cwd: &home,
            env: &env,
            homedir: Some(&wrong),
            host_defaults: None,
            host_managed_settings: None,
            host_setting_sources: None,
            settings: None,
            setting_sources: Some(&sources),
            host_enforced_settings: None,
        },
        Some(&NativeSettingSourceReader),
    );
    match kind.as_str() {
        "normal" => {
            let result = result.unwrap().unwrap();
            assert_eq!(
                result.get("agent").and_then(|v| v.get("maxOutputTokens")),
                Some(&JsValue::Number(42.0))
            );
        }
        "fifo" | "large" => {
            let error = result.unwrap_err();
            assert_eq!(error.code, "SDK_SETTING_SOURCE_RESOURCE_LIMIT");
        }
        "directory" => {
            let error = result.unwrap_err();
            assert_eq!(error.code, "SDK_SETTING_SOURCE_READ_FAILED");
            assert_eq!(
                error.message,
                "Unable to read user settings source: EISDIR: illegal operation on a directory, read"
            );
        }
        _ => panic!("invalid owned case"),
    }
    assert!(!wrong.exists());
    crate::test_process::receipt(&kind);
}
#[test]
fn compose_settings_uses_runtime_model_rules_limits_thinking_and_subagents() {
    let base = crate::metadata::object([
        (
            "model",
            crate::metadata::object([("provider", "p".into()), ("model", "base".into())]),
        ),
        ("maxOutputTokens", JsValue::Number(10.0)),
        (
            "subagents",
            crate::metadata::object([
                (
                    "default",
                    crate::metadata::object([
                        ("provider", "p".into()),
                        ("model", "old-child".into()),
                    ]),
                ),
                ("maxDepth", JsValue::Number(2.0)),
            ]),
        ),
    ]);
    let sdk = crate::metadata::object([(
        "settings",
        crate::metadata::object([(
            "agent",
            crate::metadata::object([
                ("model", "p/chosen".into()),
                ("maxContextTokens", JsValue::Number(44.0)),
                ("maxOutputTokens", JsValue::Number(22.0)),
                ("thinking", JsValue::Bool(false)),
                (
                    "subagents",
                    crate::metadata::object([
                        ("default", JsValue::Null),
                        ("timeoutMs", JsValue::Number(123.0)),
                    ]),
                ),
            ]),
        )]),
    )]);
    let resolved = compose_agent(&base, Some(&sdk), &JsValue::Undefined);
    assert_eq!(
        resolved.get("model").and_then(|v| v.get("model")),
        Some(&JsValue::from("chosen"))
    );
    assert_eq!(
        resolved.get("maxOutputTokens"),
        Some(&JsValue::Number(22.0))
    );
    assert_eq!(resolved.get("thinking"), Some(&JsValue::Bool(false)));
    assert_eq!(
        resolved.get("subagents").and_then(|v| v.get("default")),
        Some(&JsValue::Null)
    );
    assert_eq!(
        resolved.get("subagents").and_then(|v| v.get("maxDepth")),
        Some(&JsValue::Number(2.0))
    );
    let short = crate::metadata::object([(
        "settings",
        crate::metadata::object([(
            "agent",
            crate::metadata::object([("model", "short-id".into())]),
        )]),
    )]);
    assert_eq!(
        compose_agent(&base, Some(&short), &JsValue::Undefined).get("model"),
        base.get("model"),
        "TS runtime keeps parent for short id even after catalog validation"
    );
    assert_eq!(compose_agent(&base, None, &JsValue::Undefined), base);
}

struct Runtime {
    root: std::path::PathBuf,
    plugins: std::sync::Arc<crate::runtime::plugins::PluginRuntime>,
}
impl crate::runtime::sdk_session::SdkRuntimeResolver for Runtime {
    fn resolve(
        &self,
        _: Option<&pilotdeck_protocol::JsString>,
    ) -> Result<
        crate::runtime::sdk_session::ResolvedSdkRuntime,
        crate::runtime::sdk_session::SdkSessionError,
    > {
        Ok(crate::runtime::sdk_session::ResolvedSdkRuntime {
            project_root: self.root.clone(),
            snapshot: crate::metadata::object([(
                "config",
                crate::metadata::object([
                    (
                        "agent",
                        crate::metadata::object([(
                            "model",
                            crate::metadata::object([
                                ("provider", "p".into()),
                                ("model", "base".into()),
                            ]),
                        )]),
                    ),
                    (
                        "model",
                        crate::metadata::object([(
                            "providers",
                            crate::metadata::object([(
                                "p",
                                crate::metadata::object([(
                                    "models",
                                    crate::metadata::object([(
                                        "base",
                                        JsValue::Object(pilotdeck_protocol::JsObject::new()),
                                    )]),
                                )]),
                            )]),
                        )]),
                    ),
                ]),
            )]),
            plugin_runtime: self.plugins.clone(),
            lease: None,
        })
    }
}
#[tokio::test(flavor = "current_thread")]
async fn sdk_settings_and_style_rpc_share_committed_owner_and_replace_clears_selection() {
    use crate::native_process::{
        assembly::sdk_adapter::NativeSdkPort, session_overrides::SdkOutputStylePort,
    };
    use crate::runtime::{
        plugins::{PluginRuntime, PluginRuntimeOptions},
        sdk_session::{SdkRuntimeResolver, SdkSessionOptions, SdkSessionRegistry},
    };
    use std::sync::Arc;
    let scratch = crate::test_scratch::Scratch::new("sdk-style-owner");
    let home = scratch.path().join("home");
    let project = scratch.path().join("project");
    std::fs::create_dir(&home).unwrap();
    std::fs::create_dir(&project).unwrap();
    std::fs::write(
        home.join("pilotdeck.yaml"),
        "agent:\n  maxOutputTokens: 42\n",
    )
    .unwrap();
    let plugins = Arc::new(PluginRuntime::new(PluginRuntimeOptions {
        project_root: project.clone(),
        pilot_home: home.clone(),
        builtin_skills_root: None,
        builtin_plugins: vec![],
        builtin_plugins_enabled: Default::default(),
    }));
    let runtime = Arc::new(Runtime {
        root: project.clone(),
        plugins: plugins.clone(),
    });
    let overrides = Arc::new(crate::native_process::SessionOverrides::default());
    let projection = Arc::downgrade(&overrides);
    let options = SdkSessionOptions::new(
        &project,
        &project,
        source_environment(&Env::new(), &home),
        runtime.clone(),
    )
    .with_homedir(home.join("must-not-read"))
    .with_setting_source_reader(Arc::new(NativeSettingSourceReader))
    .with_output_style_projection(Arc::new(move |key, name| {
        if let Some(owner) = projection.upgrade() {
            match name {
                Some(name) => owner.set_output_style(key, name),
                None => owner.clear_output_style(key),
            }
        }
    }));
    let sdk = Arc::new(NativeSdkPort {
        registry: tokio::sync::Mutex::new(SdkSessionRegistry::new(options)),
        handlers: Arc::default(),
        async_hooks: Arc::default(),
    });
    let port: Arc<dyn SdkOutputStylePort> = sdk.clone();
    overrides.bind_sdk_styles(Arc::downgrade(&port));
    let key = pilotdeck_protocol::JsString::from("style-session");
    let root_key = pilotdeck_protocol::JsString::from(project.to_string_lossy().into_owned());
    let input = crate::metadata::object([
        ("settingSources", JsValue::Array(vec!["user".into()])),
        (
            "settings",
            crate::metadata::object([(
                "agent",
                crate::metadata::object([("maxOutputTokens", JsValue::Number(84.0))]),
            )]),
        ),
        ("outputStyle", "first".into()),
        ("systemPrompt", "keep-sdk-prompt".into()),
    ]);
    tokio::time::timeout(
        std::time::Duration::from_secs(3),
        sdk.registry
            .lock()
            .await
            .set_session_config(&key, &input, Some(&root_key), None),
    )
    .await
    .unwrap()
    .unwrap();
    let resolved = {
        let runtime = runtime.resolve(None).unwrap();
        let job = sdk
            .registry
            .lock()
            .await
            .resolve_session_config_async(&key, &runtime);
        job.await.unwrap().unwrap()
    };
    assert_eq!(
        resolved
            .get("settings")
            .and_then(|s| s.get("agent"))
            .and_then(|s| s.get("maxOutputTokens")),
        Some(&JsValue::Number(84.0)),
        "SDK overlay wins actual user source in production resolver"
    );
    assert_eq!(
        overrides.effective_output_style(&key).await.unwrap(),
        Some("first".into())
    );
    assert_eq!(overrides.output_style(&key), Some("first".into()));
    overrides
        .select_output_style(&key, &root_key, Some("changed-by-rpc".into()))
        .await
        .unwrap();
    {
        let registry = sdk.registry.lock().await;
        let config = registry.session_config(&key).unwrap();
        assert_eq!(
            config.get("systemPrompt"),
            Some(&JsValue::from("keep-sdk-prompt"))
        );
        assert_eq!(
            config.get("outputStyle"),
            Some(&JsValue::from("changed-by-rpc"))
        );
        assert_eq!(config.get("settings"), input.get("settings"));
    }
    sdk.registry
        .lock()
        .await
        .set_session_config(
            &key,
            &JsValue::Object(pilotdeck_protocol::JsObject::new()),
            Some(&root_key),
            None,
        )
        .await
        .unwrap();
    assert_eq!(overrides.effective_output_style(&key).await.unwrap(), None);
    assert_eq!(
        overrides.output_style(&key),
        None,
        "empty replacement must not resurrect a stale selection cache"
    );
    drop(port);
    drop(sdk);
    assert!(
        overrides.effective_output_style(&key).await.is_err(),
        "closed SDK owner does not silently fall back to the observation cache"
    );
    plugins.dispose().await.unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn model_policy_consumes_resolved_user_source_and_short_fallback_inherits_effective_parent() {
    use crate::native_process::assembly::sdk_adapter::NativeSdkPort;
    use crate::runtime::{
        plugins::{PluginRuntime, PluginRuntimeOptions},
        sdk_session::{SdkRuntimeResolver, SdkSessionOptions, SdkSessionRegistry},
        turn::admission::SubmitSdkSessionConfig,
    };
    use std::sync::Arc;
    struct TwoModels(Runtime);
    impl SdkRuntimeResolver for TwoModels {
        fn resolve(
            &self,
            p: Option<&pilotdeck_protocol::JsString>,
        ) -> Result<
            crate::runtime::sdk_session::ResolvedSdkRuntime,
            crate::runtime::sdk_session::SdkSessionError,
        > {
            let mut r = self.0.resolve(p)?;
            r.snapshot = crate::metadata::object([(
                "config",
                crate::metadata::object([
                    (
                        "agent",
                        crate::metadata::object([(
                            "model",
                            crate::metadata::object([
                                ("provider", "p".into()),
                                ("model", "base".into()),
                            ]),
                        )]),
                    ),
                    (
                        "model",
                        crate::metadata::object([(
                            "providers",
                            crate::metadata::object([(
                                "p",
                                crate::metadata::object([(
                                    "models",
                                    crate::metadata::object([
                                        (
                                            "base",
                                            JsValue::Object(pilotdeck_protocol::JsObject::new()),
                                        ),
                                        (
                                            "selected",
                                            JsValue::Object(pilotdeck_protocol::JsObject::new()),
                                        ),
                                    ]),
                                )]),
                            )]),
                        )]),
                    ),
                ]),
            )]);
            Ok(r)
        }
    }
    let scratch = crate::test_scratch::Scratch::new("sdk-model-settings");
    let home = scratch.path().join("home");
    let project = scratch.path().join("project");
    std::fs::create_dir(&home).unwrap();
    std::fs::create_dir(&project).unwrap();
    let contents = "agent:\n  model: p/selected\n  fallbackModel: base\n";
    std::fs::write(home.join("pilotdeck.yaml"), contents).unwrap();
    let plugins = Arc::new(PluginRuntime::new(PluginRuntimeOptions {
        project_root: project.clone(),
        pilot_home: home.clone(),
        builtin_skills_root: None,
        builtin_plugins: vec![],
        builtin_plugins_enabled: Default::default(),
    }));
    let runtime = Arc::new(TwoModels(Runtime {
        root: project.clone(),
        plugins: plugins.clone(),
    }));
    let options = SdkSessionOptions::new(
        &project,
        &project,
        source_environment(&Env::new(), &home),
        runtime,
    )
    .with_homedir(home.join("unread"))
    .with_setting_source_reader(Arc::new(NativeSettingSourceReader));
    let sdk = NativeSdkPort {
        registry: tokio::sync::Mutex::new(SdkSessionRegistry::new(options)),
        handlers: Arc::default(),
        async_hooks: Arc::default(),
    };
    let session = "settings-model".into();
    let key = pilotdeck_protocol::JsString::from(project.to_string_lossy().into_owned());
    sdk.registry
        .lock()
        .await
        .set_session_config(
            &session,
            &crate::metadata::object([("settingSources", JsValue::Array(vec!["user".into()]))]),
            Some(&key),
            None,
        )
        .await
        .unwrap();
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(3),
        SubmitSdkSessionConfig::models(&sdk, &session, Some(&key)),
    )
    .await
    .unwrap()
    .unwrap()
    .unwrap();
    assert_eq!(result.fallback.len(), 1);
    assert_eq!(
        result.fallback[0].provider,
        pilotdeck_protocol::JsString::from("p")
    );
    assert_eq!(
        result.fallback[0].model,
        pilotdeck_protocol::JsString::from("selected"),
        "TS bare fallback inherits composed parent, not pre-settings project model"
    );
    assert_eq!(
        std::fs::read_to_string(home.join("pilotdeck.yaml")).unwrap(),
        contents
    );
    plugins.dispose().await.unwrap();
}
