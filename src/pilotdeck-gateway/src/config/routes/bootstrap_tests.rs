use super::*;
use pilotdeck_protocol::parse_json;
struct Root(PathBuf);
impl Root {
    fn new() -> Self {
        let p = std::env::temp_dir().join(format!("config-bootstrap-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&p).unwrap();
        std::fs::write(p.join("pilotdeck.yaml"),"schemaVersion: 1\nagent: {model: parity/deterministic}\nmodel:\n  providers:\n    parity:\n      protocol: openai\n      url: http://127.0.0.1:1/v1\n      apiKey: mock-only\n      models: {deterministic: {}}\n").unwrap();
        Self(p)
    }
}
impl Drop for Root {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
fn context() -> SessionContext {
    SessionContext {
        session_key: "s".into(),
        project_key: None,
        channel_key: "web".into(),
        allowed_tools: None,
        disallowed_tools: None,
    }
}
async fn call(host: &ConfigBootstrap, method: &str, params: &str) -> Result {
    super::super::native::call(
        host.state.clone(),
        method.into(),
        parse_json(params).unwrap(),
    )
    .await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn production_factory_and_controls_use_one_owner_and_current_session_overrides() {
    let root = Root::new();
    let host = ConfigBootstrap::new(root.0.clone(), root.0.clone(), Env::new()).unwrap();
    assert!(host.state.lock().unwrap().host_bound);
    let original = host.session(context()).await.unwrap();
    let reservation = host.owner.begin_turn(&"s".into(), &"run".into()).unwrap();
    assert_eq!(
        call(
            &host,
            "set_session_thinking",
            r#"{"sessionKey":"s","thinking":null}"#
        )
        .await
        .unwrap_err()
        .code,
        "SESSION_BUSY"
    );
    host.owner
        .end_turn(&"s".into(), Some(&"run".into()), Some(&reservation));
    drop(reservation);
    call(
        &host,
        "set_session_thinking",
        r#"{"sessionKey":"s","thinking":{"enabled":true,"mode":"high"}}"#,
    )
    .await
    .unwrap();
    assert_eq!(
        original.state(),
        crate::runtime::session::HandleState::Disposed
    );
    let session = host.session(context()).await.unwrap();
    assert_eq!(
        session.snapshot().get("thinking").unwrap().get("mode"),
        Some(&"high".into())
    );
    call(
        &host,
        "set_permission_mode",
        r#"{"sessionKey":"s","mode":"plan"}"#,
    )
    .await
    .unwrap();
    let next = host.session(context()).await.unwrap();
    assert!(next.same_identity(&session));
    assert_eq!(
        next.snapshot()
            .get("overrides")
            .unwrap()
            .get("permissionMode"),
        Some(&"plan".into())
    );
    call(
        &host,
        "apply_flag_settings",
        r#"{"sessionKey":"s","settings":{"permissions":{"defaultMode":"default"}}}"#,
    )
    .await
    .unwrap();
    let next = host.session(context()).await.unwrap();
    assert!(!next.same_identity(&session));
    assert_eq!(
        next.snapshot()
            .get("overrides")
            .unwrap()
            .get("permissionMode"),
        Some(&"default".into())
    );
    let version = next
        .snapshot()
        .get("configVersion")
        .unwrap()
        .as_number()
        .unwrap();
    call(
        &host,
        "update_settings",
        r#"{"source":"localSettings","settings":{"extension":{"includeHookEvents":true}}}"#,
    )
    .await
    .unwrap();
    let refreshed = host.session(context()).await.unwrap();
    assert!(
        refreshed
            .snapshot()
            .get("configVersion")
            .unwrap()
            .as_number()
            .unwrap()
            > version
    );
    assert!(!refreshed.same_identity(&next));
    // The production factory never re-enters CONFIG while the owner is closing.
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        let (admission, close) = tokio::join!(
            host.session(context()),
            call(
                &host,
                "set_session_thinking",
                r#"{"sessionKey":"s","thinking":null}"#
            )
        );
        admission.unwrap();
        close.unwrap();
    })
    .await
    .unwrap();
    host.close().await.unwrap();
}
#[tokio::test]
async fn production_policy_and_disk_builtin_sources_are_bound_before_requests() {
    let root = Root::new();
    let policy_path = root.0.join("trusted-policy.json");
    std::fs::write(
        &policy_path,
        r#"{"settings":{"canUpdateLocalSettings":false}}"#,
    )
    .unwrap();
    let plugins = root.0.join("extension/plugins/builtin");
    let plugin = plugins.join("host");
    std::fs::create_dir_all(plugin.join("commands")).unwrap();
    std::fs::create_dir_all(plugin.join("output-styles")).unwrap();
    std::fs::write(
        plugin.join("plugin.json"),
        r#"{"name":"host","version":"1.0.0","commands":"commands","outputStyles":"output-styles"}"#,
    )
    .unwrap();
    std::fs::write(plugin.join("commands/probe.md"), "Host command").unwrap();
    std::fs::write(plugin.join("output-styles/brief.md"), "Brief host style").unwrap();
    let skills = root.0.join("bundled-skills/host-skill");
    std::fs::create_dir_all(&skills).unwrap();
    std::fs::write(
        skills.join("SKILL.md"),
        "---\ndescription: Host skill\n---\nHost guidance",
    )
    .unwrap();
    let env = Env::from([
        (
            "PILOTDECK_ORGANIZATION_POLICY_PATH".into(),
            policy_path.to_string_lossy().into_owned(),
        ),
        (
            "PILOTDECK_BUNDLED_SKILLS_DIR".into(),
            root.0.join("bundled-skills").to_string_lossy().into_owned(),
        ),
    ]);
    let host = ConfigBootstrap::new(root.0.clone(), root.0.clone(), env).unwrap();
    assert_eq!(call(&host,"update_settings",r#"{"source":"localSettings","settings":{"extension":{"includeHookEvents":true}},"organizationPolicy":{"settings":{"canUpdateLocalSettings":true}}}"#).await.unwrap_err().code,"GATEWAY_ORGANIZATION_SETTINGS_UPDATE_DENIED");
    let styles = call(&host, "output_styles_list", "{}").await.unwrap();
    assert!(format!("{styles:?}").contains("host:brief"));
    let commands = call(
        &host,
        "commands_list",
        &serde_json::json!({"projectKey":root.0}).to_string(),
    )
    .await
    .unwrap();
    assert!(format!("{commands:?}").contains("/host:probe"));
    use crate::runtime::sdk_session::SdkRuntimeResolver;
    let runtime = host.state.lock().unwrap().resolver.resolve(None).unwrap();
    let view = runtime.plugin_runtime.create_view(vec![]).await;
    assert!(
        view.all_skills()
            .iter()
            .any(|s| s.name.contains("host-skill"))
    );
    call(&host, "reload_output_styles", "{}").await.unwrap();
    call(
        &host,
        "set_output_style",
        r#"{"sessionKey":"s","name":"host:brief"}"#,
    )
    .await
    .unwrap();
    host.session(context()).await.unwrap();
    host.close().await.unwrap();
    std::fs::write(
        &policy_path,
        r#"{"settings":{"canUpdateLocalSettings":true}}"#,
    )
    .unwrap();
    let env = Env::from([(
        "PILOTDECK_ORGANIZATION_POLICY_PATH".into(),
        policy_path.to_string_lossy().into_owned(),
    )]);
    assert_eq!(
        ConfigBootstrap::new(root.0.clone(), root.0.clone(), env)
            .err()
            .unwrap()
            .code,
        "UNSUPPORTED_GATEWAY_ORGANIZATION_POLICY"
    );
}

#[tokio::test]
async fn organization_execution_limits_are_parsed_and_invalid_values_are_rejected() {
    let root = Root::new();
    let path = root.0.join("organization-policy.json");
    std::fs::write(
        &path,
        r#"{"permissions":{"deny":["Bash(rm *)"],"ask":["Write"],"defaultMode":"plan","canPrompt":false},"models":{"allow":["p/*"]},"providers":{"allow":["p"],"origins":{"allow":["https://api.example.test"]},"credentials":{"allow":["environment"]}},"tools":{"allow":["read_*"]},"settingSources":{"allow":["managed","project"]},"limits":{"maxTurns":7,"maxBudgetUsd":2.5,"maxTaskBudgetUsd":1.5,"maxSubagentDepth":2},"settings":{"canUpdateLocalSettings":false,"maxContextTokens":8192,"maxOutputTokens":2048,"maxThinkingTokens":0,"maxSubagentTimeoutMs":900,"sessionDefaults":{"agent":{"maxContextTokens":4096}},"sessionDefaultSources":["managed","project"]}}"#,
    )
    .unwrap();
    let env = Env::from([(
        "PILOTDECK_ORGANIZATION_POLICY_PATH".into(),
        path.to_string_lossy().into_owned(),
    )]);
    let host = ConfigBootstrap::new(root.0.clone(), root.0.clone(), env.clone()).unwrap();
    let state = host.state.lock().unwrap();
    let policy = state.sdk.options().organization_policy.as_ref().unwrap();
    assert_eq!(policy.max_turns, Some(7.0));
    assert_eq!(policy.max_subagent_depth, Some(2.0));
    assert_eq!(policy.max_subagent_timeout_ms, Some(900.0));
    assert_eq!(policy.max_budget_usd, Some(2.5));
    assert_eq!(policy.max_task_budget_usd, Some(1.5));
    assert!(policy.permissions.as_ref().unwrap().default_plan);
    assert_eq!(policy.permissions.as_ref().unwrap().can_prompt, Some(false));
    assert_eq!(policy.tools.as_ref().unwrap().allow, ["read_*"]);
    assert_eq!(policy.models.as_ref().unwrap().allow, ["p/*"]);
    assert_eq!(policy.providers.as_ref().unwrap().allow, ["p"]);
    assert_eq!(
        policy.setting_sources.as_ref().unwrap().allow,
        ["managed", "project"]
    );
    let settings = policy.settings.as_ref().unwrap();
    assert_eq!(settings.max_context_tokens, Some(8192.0));
    assert_eq!(settings.max_output_tokens, Some(2048.0));
    assert_eq!(settings.max_thinking_tokens, Some(0.0));
    assert_eq!(settings.can_update_local_settings, Some(false));
    assert!(settings.session_defaults.is_some());
    assert!(settings.session_default_sources.is_some());
    drop(state);

    std::fs::write(&path, r#"{"limits":{"maxTurns":0}}"#).unwrap();
    let error = ConfigBootstrap::new(root.0.clone(), root.0.clone(), env)
        .err()
        .unwrap();
    assert_eq!(error.code, "INVALID_GATEWAY_ORGANIZATION_POLICY");
    assert_eq!(
        error.message,
        "organizationPolicy.limits.maxTurns must be a positive safe integer."
    );

    std::fs::write(&path, r#"{"limits":{"future":1}}"#).unwrap();
    let env = Env::from([(
        "PILOTDECK_ORGANIZATION_POLICY_PATH".into(),
        path.to_string_lossy().into_owned(),
    )]);
    let error = ConfigBootstrap::new(root.0.clone(), root.0.clone(), env)
        .err()
        .unwrap();
    assert_eq!(error.code, "UNSUPPORTED_GATEWAY_ORGANIZATION_POLICY");
    assert_eq!(
        error.message,
        "organizationPolicy contains an unsupported field."
    );

    std::fs::write(&path, r#"{"permissions":[]}"#).unwrap();
    let env = Env::from([(
        "PILOTDECK_ORGANIZATION_POLICY_PATH".into(),
        path.to_string_lossy().into_owned(),
    )]);
    let error = ConfigBootstrap::new(root.0.clone(), root.0.clone(), env)
        .err()
        .unwrap();
    assert_eq!(error.code, "INVALID_GATEWAY_ORGANIZATION_POLICY");
    assert_eq!(
        error.message,
        "organizationPolicy.permissions must be an object."
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn dropping_server_stops_and_uninstalls_channel_while_executor_remains_alive() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let root = Root::new();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let calls = Arc::new(AtomicUsize::new(0));
    let seen = calls.clone();
    let serving = tokio::spawn(async move {
        loop {
            let (mut socket, _) = listener.accept().await.unwrap();
            let seen = seen.clone();
            tokio::spawn(async move {
                let mut buf = [0; 8192];
                let len = socket.read(&mut buf).await.unwrap();
                seen.fetch_add(1, Ordering::SeqCst);
                let body = if String::from_utf8_lossy(&buf[..len]).contains("get_bot_qrcode") {
                    r#"{"qrcode":"mock","qrcode_img_content":"https://mock.invalid/qr"}"#
                } else {
                    r#"{"status":"wait"}"#
                };
                let _=socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).as_bytes()).await;
            });
        }
    });
    let host = ConfigBootstrap::new(
        root.0.clone(),
        root.0.clone(),
        Env::from([("PILOTDECK_WEIXIN_BASE_URL".into(), base)]),
    )
    .unwrap();
    let proxy = Arc::new(crate::http_proxy::Proxy {
        upstream: "http://127.0.0.1:1".into(),
        client: hyper_util::client::legacy::Client::builder(hyper_util::rt::TokioExecutor::new())
            .build_http(),
    });
    let server = crate::LocalServer::with_token(proxy, "isolated-host-token".into())
        .unwrap()
        .with_native_config(&host);
    call(&host, "prepare_weixin_login", "{}").await.unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        while calls.load(Ordering::SeqCst) < 2 {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    drop(server);
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        loop {
            let status: serde_json::Value = serde_json::from_slice(
                &std::fs::read(root.0.join("channels/runtime-status.json")).unwrap(),
            )
            .unwrap();
            if status["channels"]["weixin"]["state"] == "stopped" {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let count = calls.load(Ordering::SeqCst);
    tokio::time::sleep(std::time::Duration::from_millis(1150)).await;
    assert_eq!(calls.load(Ordering::SeqCst), count);
    assert!(super::super::native::installed(&root.0, &root.0).is_none());
    assert_eq!(
        call(&host, "prepare_weixin_login", "{}")
            .await
            .unwrap_err()
            .code,
        "CONFIG_HOST_STOPPED"
    );
    assert!(host.session(context()).await.is_err());
    serving.abort();
}

#[tokio::test]
async fn sidecar_turn_input_consumes_current_config_thinking_permission_and_style() {
    let root = Root::new();
    let plugin = root.0.join(".pilotdeck/plugins/style");
    std::fs::create_dir_all(plugin.join("output-styles")).unwrap();
    std::fs::write(
        plugin.join("plugin.json"),
        r#"{"name":"style","version":"1.0.0","outputStyles":"output-styles"}"#,
    )
    .unwrap();
    std::fs::write(
        plugin.join("output-styles/brief.md"),
        "Use a concise answer.",
    )
    .unwrap();
    let host = ConfigBootstrap::new(root.0.clone(), root.0.clone(), Env::new()).unwrap();
    call(
        &host,
        "set_permission_mode",
        r#"{"sessionKey":"s","mode":"plan"}"#,
    )
    .await
    .unwrap();
    call(
        &host,
        "set_session_thinking",
        r#"{"sessionKey":"s","thinking":{"enabled":true,"mode":"high"}}"#,
    )
    .await
    .unwrap();
    call(
        &host,
        "set_output_style",
        r#"{"sessionKey":"s","name":"style:brief"}"#,
    )
    .await
    .unwrap();
    host.session(context()).await.unwrap();
    let spawner = Arc::new(BinarySpawner {
        executable: "/unused".into(),
        cwd: root.0.clone(),
        env: Env::new(),
    });
    let sidecar = SidecarSession::new(
        SidecarSessionConfig {
            limits: crate::host::Limits::default(),
            session_key: "s".into(),
            project_key: None,
            cwd: root.0.to_string_lossy().as_ref().into(),
            services: None,
            snapshot: JsValue::Null,
        },
        spawner,
    );
    let session = ConfigSession {
        sidecar,
        inputs: host.inputs.clone(),
        key: "s".into(),
    };
    let value = session
        .turn_input(parse_json(r#"{"messages":[],"agent":{"provider":"untrusted"}}"#).unwrap())
        .unwrap();
    assert_eq!(
        value.get("agent").unwrap().get("provider"),
        Some(&"parity".into())
    );
    assert_eq!(
        value.get("agent").unwrap().get("model"),
        Some(&"deterministic".into())
    );
    assert_eq!(
        value
            .get("agent")
            .unwrap()
            .get("thinking")
            .unwrap()
            .get("mode"),
        Some(&"high".into())
    );
    assert_eq!(value.get("basePermissionMode"), Some(&"plan".into()));
    assert_eq!(
        value.get("agent").unwrap().get("cwd"),
        Some(&root.0.to_string_lossy().as_ref().into())
    );
    assert!(
        value
            .get("agent")
            .unwrap()
            .get("appendSystemPrompt")
            .unwrap()
            .as_string()
            .unwrap()
            .to_utf8()
            .unwrap()
            .contains("Use a concise answer.")
    );
    host.close().await.unwrap();
}

#[tokio::test]
async fn actual_baseline_builtin_assets_load_through_production_source_selection() {
    use crate::runtime::sdk_session::SdkRuntimeResolver;
    let root = Root::new();
    let builtin = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../../ts-baseline/src/extension/plugins/builtin");
    if !builtin.is_dir() {
        eprintln!("Actual TS builtin asset probe unavailable: baseline source absent");
        return;
    }
    let host = ConfigBootstrap::new(
        root.0.clone(),
        root.0.clone(),
        Env::from([(
            "PILOTDECK_BUILTIN_PLUGINS_DIR".into(),
            builtin.to_string_lossy().into_owned(),
        )]),
    )
    .unwrap();
    let runtime = host.state.lock().unwrap().resolver.resolve(None).unwrap();
    runtime.plugin_runtime.refresh().await.unwrap();
    let view = runtime.plugin_runtime.create_view(vec![]).await;
    let names: Vec<_> = view.snapshot().iter().map(|p| p.name.clone()).collect();
    assert!(names.contains(&"browser-use".into()));
    assert!(names.contains(&"funasr".into()));
    assert!(names.contains(&"windows-skills".into()));
    assert!(
        view.all_skills()
            .iter()
            .any(|s| s.name.contains("browser-use-install"))
    );
    assert!(view.mcp_servers().get("browser-use").is_some());
    assert!(view.mcp_servers().get("funasr").is_some());
    println!(
        "actual builtin source: {}; names={names:?}; skills and MCP sources loaded without execution",
        builtin.display()
    );
    host.close().await.unwrap();
}
