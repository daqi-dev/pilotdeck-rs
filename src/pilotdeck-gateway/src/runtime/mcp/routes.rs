//! MCP gateway methods over the shared SDK configuration. Unknown methods
//! remain unclaimed until their gateway validation and lifecycle are wired.
use super::sdk::{
    PluginProjectRuntime, SdkMcpConfig, SdkMcpConfigOptions, SdkMcpError, SdkMcpFuture,
    SdkMcpProject, SdkMcpProjectResolver, SystemEnvironment, require_input, required_trimmed,
};
use crate::{
    config::permission::GatewaySessionPermissionRuleSetRegistry,
    runtime::{
        plugins::{PluginRuntime, PluginRuntimeOptions},
        project::{
            DisposeFuture, ProjectRuntimeRegistry, ResourceBundle, RuntimeError, RuntimeResources,
        },
    },
};
use pilotdeck_protocol::{JsString, JsValue};
use std::{future::Future, path::PathBuf, sync::Arc};

/// The same live router that admits turns and owns session teardown.
pub trait McpSessionRouter: Send + Sync {
    fn has_active_turn(&self, key: &JsString) -> bool;
    fn close<'a>(&'a self, key: &'a JsString) -> SdkMcpFuture<'a, Result<(), SdkMcpError>>;
}

impl McpSessionRouter for crate::runtime::session::SessionOwner {
    fn has_active_turn(&self, key: &JsString) -> bool {
        self.has_active_turn(key)
    }

    fn close<'a>(&'a self, key: &'a JsString) -> SdkMcpFuture<'a, Result<(), SdkMcpError>> {
        let closed = self.close(key);
        Box::pin(async move {
            closed
                .await
                .map_err(|error| SdkMcpError::plain("Error", error.message))
        })
    }
}

pub struct McpRoutes {
    sdk: Arc<SdkMcpConfig>,
    router: Option<Arc<dyn McpSessionRouter>>,
}

impl McpRoutes {
    pub fn sdk_config(&self) -> Arc<SdkMcpConfig> {
        self.sdk.clone()
    }
    /// The host supplies the same SDK state to every request and connection.
    pub fn new(sdk: Arc<SdkMcpConfig>) -> Self {
        Self { sdk, router: None }
    }

    /// SDK close_session must perform the host's full inner close; this router
    /// supplies busy admission and the additional outer close on that same host.
    pub fn with_session_router(sdk: Arc<SdkMcpConfig>, router: Arc<dyn McpSessionRouter>) -> Self {
        Self {
            sdk,
            router: Some(router),
        }
    }

    pub fn handles(method: &str) -> bool {
        matches!(
            method,
            "mcp_server_status"
                | "set_mcp_servers"
                | "mcp_server_reconnect"
                | "mcp_server_toggle"
                | "set_mcp_permission_mode_override"
        )
    }

    pub fn handle<'a>(
        &'a self,
        method: &'a str,
        params: &'a JsValue,
    ) -> Option<impl Future<Output = Result<JsValue, SdkMcpError>> + Send + 'a> {
        Self::handles(method).then_some(async move {
            if matches!(method, "mcp_server_reconnect" | "mcp_server_toggle" | "set_mcp_permission_mode_override") {
                let toggle = method == "mcp_server_toggle";
                let permission = method == "set_mcp_permission_mode_override";
                let router = self.router.as_ref().ok_or_else(|| {
                    SdkMcpError::gateway(
                        "CAPABILITY_UNAVAILABLE",
                        format!("{method} is unavailable."),
                    )
                })?;
                let params = require_input(params, "sessionKey")?;
                let key = required_trimmed(
                    params.get("sessionKey"),
                    "sessionKey",
                    "INVALID_MCP_SERVER",
                    "sessionKey and serverName are required.",
                )?;
                required_trimmed(
                    params.get("serverName"),
                    "serverName",
                    "INVALID_MCP_SERVER",
                    "sessionKey and serverName are required.",
                )?;
                if permission {
                    let undefined = JsValue::Undefined;
                    let mode = params.get("mode").unwrap_or(&undefined);
                    if !matches!(mode, JsValue::Null) && !matches!(mode, JsValue::String(value) if value.equals_str("default") || value.equals_str("auto")) {
                        let mut message = "Unsupported MCP permission mode: ".encode_utf16().collect::<Vec<_>>();
                        message.extend(crate::js_value::to_string_units_checked(mode)
                            .map_err(|error| SdkMcpError::plain(error.name(), error.message()))?);
                        return Err(SdkMcpError::gateway("INVALID_MCP_PERMISSION_MODE", JsString::from_utf16(message)));
                    }
                }
                if router.has_active_turn(&key) {
                    return Err(SdkMcpError::gateway(
                        "SESSION_BUSY",
                        if permission {
                            "Cannot change MCP permission mode while a turn is active."
                        } else if toggle {
                            "Cannot toggle MCP while a turn is active."
                        } else {
                            "Cannot reconnect MCP while a turn is active."
                        },
                    ));
                }
                if permission {
                    let result = self.sdk.set_mcp_permission_mode_override(params).await?;
                    router.close(&key).await?;
                    return Ok(result);
                } else if toggle {
                    self.sdk.toggle_mcp_server(params).await?;
                } else {
                    self.sdk.reconnect_mcp_server(params).await?;
                }
                router.close(&key).await?;
                return Ok(crate::metadata::object([("ok", JsValue::Bool(true))]));
            }
            if method == "set_mcp_servers" {
                let router = self.router.as_ref().ok_or_else(|| {
                    SdkMcpError::gateway(
                        "CAPABILITY_UNAVAILABLE",
                        "set_mcp_servers is unavailable.",
                    )
                })?;
                let params = require_input(params, "sessionKey")?;
                let key = required_trimmed(
                    params.get("sessionKey"),
                    "sessionKey",
                    "INVALID_SESSION_KEY",
                    "sessionKey is required.",
                )?;
                if router.has_active_turn(&key) {
                    return Err(SdkMcpError::gateway(
                        "SESSION_BUSY",
                        "Cannot change MCP servers while a turn is active.",
                    ));
                }
                check_project_key(params)?;
                let result = self.sdk.set_mcp_servers(params).await?;
                router.close(&key).await?;
                return Ok(result);
            }
            check_project_key(params)?;
            self.sdk.mcp_server_status(params).await
        })
    }

    /// Read-only native assembly, with explicit storage and project roots.
    /// Construct inside the server's Tokio runtime.
    pub fn native(pilot_home: PathBuf, project_root: PathBuf) -> Self {
        let registry = ProjectRuntimeRegistry::new(
            project_root,
            Arc::new(
                move |root: &std::path::Path, resources: &mut ResourceBundle| {
                    resources.add(ProjectPlugins(Arc::new(PluginRuntime::new(
                        PluginRuntimeOptions {
                            project_root: root.into(),
                            pilot_home: pilot_home.clone(),
                            builtin_skills_root: None,
                            builtin_plugins: Vec::new(),
                            builtin_plugins_enabled: Default::default(),
                        },
                    ))));
                    Ok(())
                },
            ),
        );
        Self::new(Arc::new(SdkMcpConfig::new(SdkMcpConfigOptions {
            resolver: Arc::new(NativeProjects(registry)),
            // Writes are rejected without an injected live session router.
            close_session: Arc::new(|_, _| {
                Box::pin(async {
                    Err(SdkMcpError::gateway(
                        "CAPABILITY_UNAVAILABLE",
                        "MCP session lifecycle is unavailable on this local gateway.",
                    ))
                })
            }),
            session_plugins: Arc::new(|_: &JsValue| Vec::new()),
            permission_rule_sets: Arc::new(GatewaySessionPermissionRuleSetRegistry::new()),
            environment: Arc::new(SystemEnvironment),
            projects: crate::runtime::sdk_session::SdkSessionProjects::shared(),
        })))
    }
}

fn check_project_key(params: &JsValue) -> Result<(), SdkMcpError> {
    let Some(key) = params.get("projectKey") else {
        return Ok(());
    };
    if matches!(key, JsValue::Undefined | JsValue::Null | JsValue::String(_)) {
        return Ok(());
    }
    // The typed SDK resolver cannot receive Node path.resolve's invalid inputs.
    let received = match crate::metadata::argument_received(key) {
        crate::metadata::ArgumentReceived::Text(text) => text,
        crate::metadata::ArgumentReceived::Throws { name, message } => {
            return Err(SdkMcpError::plain(name, message));
        }
    };
    let mut message = "The \"paths[0]\" argument must be of type string. Received "
        .encode_utf16()
        .collect::<Vec<_>>();
    message.extend_from_slice(received.units());
    Err(SdkMcpError::type_error_with_code(
        "ERR_INVALID_ARG_TYPE",
        JsString::from_utf16(message),
    ))
}

struct ProjectPlugins(Arc<PluginRuntime>);
impl RuntimeResources for ProjectPlugins {
    fn dispose(&self) -> DisposeFuture<'_> {
        Box::pin(async {
            self.0
                .dispose()
                .await
                .map_err(|error| RuntimeError::new(error.message))
        })
    }
}

struct NativeProjects(ProjectRuntimeRegistry);
impl SdkMcpProjectResolver for NativeProjects {
    fn resolve<'a>(
        &'a self,
        project_key: Option<&'a JsString>,
    ) -> SdkMcpFuture<'a, Result<Arc<dyn SdkMcpProject>, SdkMcpError>> {
        Box::pin(async move {
            let key = project_key.map(|key| PathBuf::from(String::from_utf16_lossy(key.units())));
            let runtime = self
                .0
                .resolve(key.as_deref())
                .map_err(|error| SdkMcpError::plain("Error", error.message))?;
            let plugins = runtime
                .resources()
                .get::<ProjectPlugins>()
                .expect("native MCP factory installs project plugins");
            Ok(Arc::new(PluginProjectRuntime::from_generation(
                &runtime,
                plugins.0.clone(),
            )) as Arc<dyn SdkMcpProject>)
        })
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::runtime::plugins::{LoadedPlugin, PluginSource};
    use pilotdeck_protocol::parse_json;
    use std::sync::{
        Mutex,
        atomic::{AtomicBool, Ordering},
    };
    use tokio::sync::Notify;

    #[derive(Default)]
    struct Project {
        calls: Mutex<Vec<String>>,
        fail: AtomicBool,
        inner_fail: AtomicBool,
        permissions: Arc<GatewaySessionPermissionRuleSetRegistry>,
        refresh_gate: Mutex<Option<Arc<Notify>>>,
        refresh_started: Notify,
    }
    impl SdkMcpProject for Project {
        fn project_root(&self) -> JsString {
            "/mcp-test".into()
        }
        fn refresh<'a>(&'a self) -> SdkMcpFuture<'a, Result<(), SdkMcpError>> {
            Box::pin(async {
                self.calls.lock().unwrap().push("refresh".into());
                self.refresh_started.notify_one();
                let gate = self.refresh_gate.lock().unwrap().clone();
                if let Some(gate) = gate {
                    gate.notified().await;
                }
                if self.fail.load(Ordering::SeqCst) {
                    return Err(SdkMcpError::plain("Error", "refresh failed"));
                }
                self.calls.lock().unwrap().push("refreshed".into());
                Ok(())
            })
        }
        fn snapshot<'a>(&'a self) -> SdkMcpFuture<'a, Vec<Arc<LoadedPlugin>>> {
            self.calls.lock().unwrap().push("snapshot".into());
            Box::pin(async { Vec::new() })
        }
    }
    struct Resolver(Arc<Project>);
    impl SdkMcpProjectResolver for Resolver {
        fn resolve<'a>(
            &'a self,
            project_key: Option<&'a JsString>,
        ) -> SdkMcpFuture<'a, Result<Arc<dyn SdkMcpProject>, SdkMcpError>> {
            self.0
                .calls
                .lock()
                .unwrap()
                .push(format!("resolve:{project_key:?}"));
            let project: Arc<dyn SdkMcpProject> = self.0.clone();
            Box::pin(async { Ok(project) })
        }
    }

    pub(crate) fn fixture() -> (Arc<SdkMcpConfig>, Arc<McpRoutes>) {
        let (sdk, routes, _) = setup();
        (sdk, routes)
    }
    fn setup() -> (Arc<SdkMcpConfig>, Arc<McpRoutes>, Arc<Project>) {
        let project = Arc::new(Project::default());
        let observed = project.clone();
        let closing = project.clone();
        let sdk = Arc::new(SdkMcpConfig::new(SdkMcpConfigOptions {
            resolver: Arc::new(Resolver(project.clone())),
            close_session: Arc::new(move |key, reason| {
                let closing = closing.clone();
                Box::pin(async move {
                    closing
                        .calls
                        .lock()
                        .unwrap()
                        .push(format!("inner:{key:?}:{reason}"));
                    if closing.inner_fail.load(Ordering::SeqCst) {
                        return Err(SdkMcpError::plain("Error", "inner failed"));
                    }
                    Ok(())
                })
            }),
            session_plugins: Arc::new(move |key: &JsValue| {
                observed
                    .calls
                    .lock()
                    .unwrap()
                    .push("session_plugins".into());
                if key != &JsValue::from("s") {
                    return Vec::new();
                }
                vec![Arc::new(LoadedPlugin {
                    name: "injected".into(),
                    path: "/mcp-test/plugin".into(),
                    source: PluginSource::Builtin,
                    commands: Vec::new(),
                    skills: Vec::new(),
                    output_styles: Vec::new(),
                    hooks_config: None,
                    mcp_servers: Some(json(r#"{"plugin":{"command":"unused"}}"#)),
                    lsp_servers: None,
                    router_contributions: Vec::new(),
                    dispose: None,
                })]
            }),
            permission_rule_sets: project.permissions.clone(),
            projects: crate::runtime::sdk_session::SdkSessionProjects::shared(),
            environment: Arc::new(SystemEnvironment),
        }));
        (sdk.clone(), Arc::new(McpRoutes::new(sdk)), project)
    }
    fn json(text: &str) -> JsValue {
        parse_json(text).unwrap()
    }

    #[derive(Default)]
    struct Router {
        project: Arc<Project>,
        busy: AtomicBool,
        close_gate: Mutex<Option<Arc<Notify>>>,
        close_started: Notify,
    }
    impl McpSessionRouter for Router {
        fn has_active_turn(&self, key: &JsString) -> bool {
            self.project
                .calls
                .lock()
                .unwrap()
                .push(format!("busy:{key:?}"));
            self.busy.load(Ordering::SeqCst)
        }
        fn close<'a>(&'a self, key: &'a JsString) -> SdkMcpFuture<'a, Result<(), SdkMcpError>> {
            Box::pin(async move {
                self.project
                    .calls
                    .lock()
                    .unwrap()
                    .push(format!("outer:{key:?}"));
                self.close_started.notify_one();
                let gate = self.close_gate.lock().unwrap().clone();
                if let Some(gate) = gate {
                    gate.notified().await;
                }
                Ok(())
            })
        }
    }
    fn writable() -> (Arc<McpRoutes>, Arc<Router>) {
        let (sdk, _, project) = setup();
        let router = Arc::new(Router {
            project,
            ..Default::default()
        });
        (
            Arc::new(McpRoutes::with_session_router(sdk, router.clone())),
            router,
        )
    }

    #[tokio::test]
    async fn permission_modes_share_registry_preserve_keys_and_only_await_outer_close() {
        let (routes, router) = writable();
        let key = JsString::from(" s ");
        let lease = router.project.permissions.acquire(&key, None);
        let other = router
            .project
            .permissions
            .acquire(&JsString::from("s"), None);
        for (mode, count) in [(r#""auto""#, 1), (r#""default""#, 1), ("null", 0)] {
            router.project.calls.lock().unwrap().clear();
            let input = json(&format!(
                r#"{{"sessionKey":" s ","serverName":" a ","mode":{mode}}}"#
            ));
            let gate = Arc::new(Notify::new());
            *router.close_gate.lock().unwrap() = Some(gate.clone());
            let job_routes = routes.clone();
            let job = tokio::spawn(async move {
                job_routes
                    .handle("set_mcp_permission_mode_override", &input)
                    .unwrap()
                    .await
            });
            tokio::time::timeout(
                std::time::Duration::from_secs(3),
                router.close_started.notified(),
            )
            .await
            .unwrap();
            assert!(!job.is_finished());
            assert_eq!(
                *router.project.calls.lock().unwrap(),
                vec![format!("busy:{key:?}"), format!("outer:{key:?}")]
            );
            assert_eq!(lease.rules().snapshot().ask.len(), count);
            assert!(other.rules().snapshot().ask.is_empty());
            gate.notify_one();
            let result = job.await.unwrap().unwrap();
            if mode == r#""auto""# {
                assert_eq!(
                    result,
                    crate::metadata::object([(
                        "warning",
                        super::super::sdk::AUTO_PERMISSION_WARNING.into()
                    )])
                );
            } else {
                assert_eq!(result, json("{}"));
            }
        }
    }

    #[tokio::test]
    async fn permission_invalid_mode_precedes_busy_and_conversion_errors_do_not_close() {
        let (routes, router) = writable();
        router.busy.store(true, Ordering::SeqCst);
        let method = "set_mcp_permission_mode_override";
        for input in [
            r#"{"sessionKey":" ","mode":42}"#,
            r#"{"sessionKey":"s","serverName":" ","mode":42}"#,
        ] {
            assert_eq!(
                routes
                    .handle(method, &json(input))
                    .unwrap()
                    .await
                    .unwrap_err()
                    .code(),
                Some("INVALID_MCP_SERVER")
            );
        }
        for (mode, text) in [
            ("42", "42"),
            ("true", "true"),
            (r#"" auto ""#, " auto "),
            ("[]", ""),
            ("{}", "[object Object]"),
        ] {
            let input = json(&format!(
                r#"{{"sessionKey":"s","serverName":"a","mode":{mode}}}"#
            ));
            assert_eq!(
                routes.handle(method, &input).unwrap().await.unwrap_err(),
                SdkMcpError::gateway(
                    "INVALID_MCP_PERMISSION_MODE",
                    format!("Unsupported MCP permission mode: {text}")
                )
            );
        }
        assert_eq!(
            routes
                .handle(method, &json(r#"{"sessionKey":"s","serverName":"a"}"#))
                .unwrap()
                .await
                .unwrap_err()
                .message(),
            &JsString::from("Unsupported MCP permission mode: undefined")
        );
        let input =
            json(r#"{"sessionKey":"s","serverName":"a","mode":{"toString":null,"valueOf":null}}"#);
        let conversion =
            crate::js_value::to_string_units_checked(input.get("mode").unwrap()).unwrap_err();
        assert_eq!(
            routes.handle(method, &input).unwrap().await.unwrap_err(),
            SdkMcpError::plain(conversion.name(), conversion.message())
        );
        assert!(router.project.calls.lock().unwrap().is_empty());
        assert_eq!(router.project.permissions.size(), 0);
        assert_eq!(
            routes
                .handle(
                    method,
                    &json(r#"{"sessionKey":"s","serverName":"a","mode":null}"#)
                )
                .unwrap()
                .await
                .unwrap_err(),
            SdkMcpError::gateway(
                "SESSION_BUSY",
                "Cannot change MCP permission mode while a turn is active."
            )
        );
        assert_eq!(router.project.calls.lock().unwrap().len(), 1);
        assert_eq!(router.project.permissions.size(), 0);
    }

    #[tokio::test]
    async fn toggle_preserves_raw_enabled_and_orders_awaited_closes() {
        let (routes, router) = writable();
        let config =
            json(r#"{"sessionKey":" s ","servers":{" a ":{"type":"stdio","command":"node"}}}"#);
        routes.sdk.set_mcp_servers(&config).await.unwrap();
        for (enabled, expected) in [
            ("false", "disabled"),
            ("true", "configured"),
            ("0", "disabled"),
            (r#""false""#, "configured"),
            ("[]", "configured"),
            ("null", "disabled"),
        ] {
            router.project.calls.lock().unwrap().clear();
            let gate = Arc::new(Notify::new());
            *router.close_gate.lock().unwrap() = Some(gate.clone());
            let input = json(&format!(
                r#"{{"sessionKey":" s ","serverName":" a ","enabled":{enabled}}}"#
            ));
            let job_routes = routes.clone();
            let job = tokio::spawn(async move {
                job_routes
                    .handle("mcp_server_toggle", &input)
                    .unwrap()
                    .await
            });
            tokio::time::timeout(
                std::time::Duration::from_secs(3),
                router.close_started.notified(),
            )
            .await
            .unwrap();
            assert!(!job.is_finished());
            assert_eq!(
                *router.project.calls.lock().unwrap(),
                vec![
                    format!("busy:{:?}", JsString::from(" s ")),
                    format!("inner:{:?}:sdk_mcp_server_toggled", JsValue::from(" s ")),
                    format!("outer:{:?}", JsString::from(" s ")),
                ]
            );
            gate.notify_one();
            assert_eq!(job.await.unwrap().unwrap(), json(r#"{"ok":true}"#));
            let status_input = json(r#"{"sessionKey":" s "}"#);
            let expected_status = json(&format!(
                r#"{{"servers":[{{"name":" a ","status":"{expected}"}}]}}"#
            ));
            assert_eq!(
                routes
                    .handle("mcp_server_status", &status_input)
                    .unwrap()
                    .await
                    .unwrap(),
                expected_status
            );
            // Re-set uses raw `enabled ?? true`: null becomes true, numeric zero stays disabled.
            routes.sdk.set_mcp_servers(&config).await.unwrap();
            let reset_expected = if enabled == "null" {
                json(r#"{"servers":[{"name":" a ","status":"configured"}]}"#)
            } else {
                expected_status
            };
            assert_eq!(
                routes
                    .handle("mcp_server_status", &status_input)
                    .unwrap()
                    .await
                    .unwrap(),
                reset_expected
            );
        }
    }

    #[tokio::test]
    async fn toggle_validation_busy_not_found_and_inner_errors_never_outer_close() {
        let (routes, router) = writable();
        for input in [
            r#"{"sessionKey":" ","serverName":42}"#,
            r#"{"sessionKey":"s"}"#,
        ] {
            assert_eq!(
                routes
                    .handle("mcp_server_toggle", &json(input))
                    .unwrap()
                    .await
                    .unwrap_err()
                    .code(),
                Some("INVALID_MCP_SERVER")
            );
        }
        assert_eq!(
            routes
                .handle(
                    "mcp_server_toggle",
                    &json(r#"{"sessionKey":"s","serverName":false}"#)
                )
                .unwrap()
                .await
                .unwrap_err(),
            SdkMcpError::type_error("input.serverName?.trim is not a function")
        );
        assert!(router.project.calls.lock().unwrap().is_empty());
        let input = json(r#"{"sessionKey":"s","serverName":"a","enabled":false}"#);
        router.busy.store(true, Ordering::SeqCst);
        assert_eq!(
            routes
                .handle("mcp_server_toggle", &input)
                .unwrap()
                .await
                .unwrap_err(),
            SdkMcpError::gateway("SESSION_BUSY", "Cannot toggle MCP while a turn is active.")
        );
        assert_eq!(router.project.calls.lock().unwrap().len(), 1);
        router.busy.store(false, Ordering::SeqCst);
        let expected = routes.sdk.toggle_mcp_server(&input).await.unwrap_err();
        assert_eq!(expected.code(), Some("MCP_SERVER_NOT_FOUND"));
        assert_eq!(
            routes
                .handle("mcp_server_toggle", &input)
                .unwrap()
                .await
                .unwrap_err(),
            expected
        );
        assert_eq!(router.project.calls.lock().unwrap().len(), 2);
        routes
            .sdk
            .set_mcp_servers(&json(
                r#"{"sessionKey":"s","servers":{"a":{"type":"stdio","command":"node"}}}"#,
            ))
            .await
            .unwrap();
        router.project.calls.lock().unwrap().clear();
        router.busy.store(true, Ordering::SeqCst);
        assert_eq!(
            routes
                .handle("mcp_server_toggle", &input)
                .unwrap()
                .await
                .unwrap_err()
                .code(),
            Some("SESSION_BUSY")
        );
        assert_eq!(
            routes
                .sdk
                .mcp_server_status(&json(r#"{"sessionKey":"s"}"#))
                .await
                .unwrap(),
            json(
                r#"{"servers":[{"name":"plugin","status":"configured"},{"name":"a","status":"configured"}]}"#
            )
        );
        router.busy.store(false, Ordering::SeqCst);
        router.project.calls.lock().unwrap().clear();
        router.project.inner_fail.store(true, Ordering::SeqCst);
        assert_eq!(
            routes
                .handle("mcp_server_toggle", &input)
                .unwrap()
                .await
                .unwrap_err()
                .message(),
            &JsString::from("inner failed")
        );
        let calls = router.project.calls.lock().unwrap();
        assert_eq!(calls.len(), 2);
        assert!(calls[1].starts_with("inner:"));
    }

    #[tokio::test]
    async fn reconnect_validates_before_busy_and_preserves_domain_errors() {
        let (routes, router) = writable();
        for (input, code, message) in [
            (
                "null",
                None,
                "Cannot read properties of null (reading 'sessionKey')",
            ),
            (
                "{}",
                Some("INVALID_MCP_SERVER"),
                "sessionKey and serverName are required.",
            ),
            (
                r#"{"sessionKey":" ","serverName":42}"#,
                Some("INVALID_MCP_SERVER"),
                "sessionKey and serverName are required.",
            ),
            (
                r#"{"sessionKey":"s","serverName":" \ufeff"}"#,
                Some("INVALID_MCP_SERVER"),
                "sessionKey and serverName are required.",
            ),
            (
                r#"{"sessionKey":false}"#,
                None,
                "input.sessionKey?.trim is not a function",
            ),
            (
                r#"{"sessionKey":"s","serverName":42}"#,
                None,
                "input.serverName?.trim is not a function",
            ),
        ] {
            let error = routes
                .handle("mcp_server_reconnect", &json(input))
                .unwrap()
                .await
                .unwrap_err();
            assert_eq!(error.code(), code);
            assert_eq!(error.message(), &JsString::from(message));
        }
        assert!(router.project.calls.lock().unwrap().is_empty());
        let input = json(r#"{"sessionKey":"s","serverName":"a"}"#);
        router.busy.store(true, Ordering::SeqCst);
        let error = routes
            .handle("mcp_server_reconnect", &input)
            .unwrap()
            .await
            .unwrap_err();
        assert_eq!(error.code(), Some("SESSION_BUSY"));
        assert_eq!(
            error.message(),
            &JsString::from("Cannot reconnect MCP while a turn is active.")
        );
        assert_eq!(router.project.calls.lock().unwrap().len(), 1);
        router.busy.store(false, Ordering::SeqCst);
        let expected = routes.sdk.reconnect_mcp_server(&input).await.unwrap_err();
        assert_eq!(
            routes
                .handle("mcp_server_reconnect", &input)
                .unwrap()
                .await
                .unwrap_err(),
            expected
        );
        assert_eq!(expected.code(), Some("MCP_SERVER_NOT_FOUND"));
        routes
            .sdk
            .set_mcp_servers(&json(
                r#"{"sessionKey":"s","servers":{"a":{"type":"stdio","command":"node"}}}"#,
            ))
            .await
            .unwrap();
        routes
            .sdk
            .toggle_mcp_server(&json(
                r#"{"sessionKey":"s","serverName":"a","enabled":false}"#,
            ))
            .await
            .unwrap();
        router.project.calls.lock().unwrap().clear();
        let expected = routes.sdk.reconnect_mcp_server(&input).await.unwrap_err();
        assert_eq!(
            routes
                .handle("mcp_server_reconnect", &input)
                .unwrap()
                .await
                .unwrap_err(),
            expected
        );
        assert_eq!(expected.code(), Some("MCP_SERVER_DISABLED"));
        assert_eq!(router.project.calls.lock().unwrap().len(), 1);
        let (_, readonly, _) = setup();
        assert_eq!(
            readonly
                .handle("mcp_server_reconnect", &JsValue::Null)
                .unwrap()
                .await
                .unwrap_err()
                .code(),
            Some("CAPABILITY_UNAVAILABLE")
        );
    }

    #[tokio::test]
    async fn reconnect_preserves_keys_awaits_outer_close_and_stops_on_inner_error() {
        let (routes, router) = writable();
        routes
            .sdk
            .set_mcp_servers(&json(
                r#"{"sessionKey":" s ","servers":{" a ":{"type":"stdio","command":"node"}}}"#,
            ))
            .await
            .unwrap();
        router.project.calls.lock().unwrap().clear();
        let input = json(r#"{"sessionKey":" s ","serverName":" a "}"#);
        let gate = Arc::new(Notify::new());
        *router.close_gate.lock().unwrap() = Some(gate.clone());
        let job_routes = routes.clone();
        let job_input = input.clone();
        let job = tokio::spawn(async move {
            job_routes
                .handle("mcp_server_reconnect", &job_input)
                .unwrap()
                .await
        });
        tokio::time::timeout(
            std::time::Duration::from_secs(3),
            router.close_started.notified(),
        )
        .await
        .unwrap();
        assert!(!job.is_finished());
        assert_eq!(
            *router.project.calls.lock().unwrap(),
            vec![
                format!("busy:{:?}", JsString::from(" s ")),
                format!("inner:{:?}:sdk_mcp_server_reconnect", JsValue::from(" s ")),
                format!("outer:{:?}", JsString::from(" s ")),
            ]
        );
        gate.notify_one();
        assert_eq!(job.await.unwrap().unwrap(), json(r#"{"ok":true}"#));
        router.project.calls.lock().unwrap().clear();
        router.project.inner_fail.store(true, Ordering::SeqCst);
        let error = routes
            .handle("mcp_server_reconnect", &input)
            .unwrap()
            .await
            .unwrap_err();
        assert_eq!(error.message(), &JsString::from("inner failed"));
        let calls = router.project.calls.lock().unwrap();
        assert_eq!(calls.len(), 2);
        assert!(calls[1].starts_with("inner:"));
    }

    #[tokio::test]
    async fn set_validates_session_and_busy_before_any_domain_side_effect() {
        let (routes, router) = writable();
        for (input, code, message) in [
            (
                "null",
                None,
                "Cannot read properties of null (reading 'sessionKey')",
            ),
            ("{}", Some("INVALID_SESSION_KEY"), "sessionKey is required."),
            (
                r#"{"sessionKey":" \ufeff \n"}"#,
                Some("INVALID_SESSION_KEY"),
                "sessionKey is required.",
            ),
            (
                r#"{"sessionKey":42}"#,
                None,
                "input.sessionKey?.trim is not a function",
            ),
        ] {
            let error = routes
                .handle("set_mcp_servers", &json(input))
                .unwrap()
                .await
                .unwrap_err();
            assert_eq!(error.code(), code);
            assert_eq!(error.message(), &JsString::from(message));
        }
        assert!(router.project.calls.lock().unwrap().is_empty());
        router.busy.store(true, Ordering::SeqCst);
        let error = routes
            .handle(
                "set_mcp_servers",
                &json(r#"{"sessionKey":" s ","projectKey":42}"#),
            )
            .unwrap()
            .await
            .unwrap_err();
        assert_eq!(error.code(), Some("SESSION_BUSY"));
        assert_eq!(router.project.calls.lock().unwrap().len(), 1);
        let (_, readonly, _) = setup();
        assert_eq!(
            readonly
                .handle("set_mcp_servers", &JsValue::Null)
                .unwrap()
                .await
                .unwrap_err()
                .code(),
            Some("CAPABILITY_UNAVAILABLE")
        );
    }

    #[tokio::test]
    async fn set_preserves_key_orders_inner_then_outer_and_awaits_close_before_return() {
        let (routes, router) = writable();
        let gate = Arc::new(Notify::new());
        *router.close_gate.lock().unwrap() = Some(gate.clone());
        let job_routes = routes.clone();
        let job = tokio::spawn(async move {
            job_routes
                .handle(
                    "set_mcp_servers",
                    &json(
                        r#"{"sessionKey":" s ","servers":{"a":{"type":"stdio","command":"node"}}}"#,
                    ),
                )
                .unwrap()
                .await
        });
        tokio::time::timeout(
            std::time::Duration::from_secs(3),
            router.close_started.notified(),
        )
        .await
        .unwrap();
        assert!(!job.is_finished());
        let calls = router.project.calls.lock().unwrap().clone();
        assert_eq!(calls[0], format!("busy:{:?}", JsString::from(" s ")));
        assert!(calls[calls.len() - 2].starts_with("inner:"));
        assert!(calls[calls.len() - 2].starts_with(&format!("inner:{:?}:", JsValue::from(" s "))));
        assert_eq!(
            calls.last().unwrap(),
            &format!("outer:{:?}", JsString::from(" s "))
        );
        gate.notify_one();
        let result = job.await.unwrap().unwrap();
        assert_eq!(result.get("added"), Some(&json(r#"["a"]"#)));
        let status = routes
            .handle("mcp_server_status", &json(r#"{"sessionKey":" s "}"#))
            .unwrap()
            .await
            .unwrap();
        assert_eq!(
            status,
            json(r#"{"servers":[{"name":"a","status":"configured"}]}"#)
        );
        assert_eq!(
            routes
                .handle("mcp_server_status", &json(r#"{"sessionKey":"s"}"#))
                .unwrap()
                .await
                .unwrap(),
            json(r#"{"servers":[{"name":"plugin","status":"configured"}]}"#)
        );
    }

    #[tokio::test]
    async fn set_diagnostics_close_outer_but_exceptions_do_not() {
        let (routes, router) = writable();
        for servers in [
            r#"{"plugin":{"type":"stdio","command":"node"}}"#,
            r#"{"bad":{}}"#,
        ] {
            router.project.calls.lock().unwrap().clear();
            let result = routes
                .handle(
                    "set_mcp_servers",
                    &json(&format!(r#"{{"sessionKey":"s","servers":{servers}}}"#)),
                )
                .unwrap()
                .await
                .unwrap();
            assert!(
                matches!(result.get("errors"), Some(JsValue::Array(errors)) if errors.len() == 1)
            );
            let calls = router.project.calls.lock().unwrap();
            assert!(calls.last().unwrap().starts_with("outer:"));
            assert!(!calls.iter().any(|call| call.starts_with("inner:")));
        }
        for inner in [false, true] {
            router.project.calls.lock().unwrap().clear();
            router.project.fail.store(!inner, Ordering::SeqCst);
            router.project.inner_fail.store(inner, Ordering::SeqCst);
            let error = routes
                .handle(
                    "set_mcp_servers",
                    &json(
                        r#"{"sessionKey":"s","servers":{"a":{"type":"stdio","command":"node"}}}"#,
                    ),
                )
                .unwrap()
                .await
                .unwrap_err();
            assert_eq!(
                error.message(),
                &JsString::from(if inner {
                    "inner failed"
                } else {
                    "refresh failed"
                })
            );
            assert!(
                !router
                    .project
                    .calls
                    .lock()
                    .unwrap()
                    .iter()
                    .any(|call| call.starts_with("outer:"))
            );
        }
    }

    #[tokio::test]
    async fn status_preserves_params_and_waits_for_refresh_before_session_plugins() {
        let (_, routes, project) = setup();
        let gate = Arc::new(Notify::new());
        *project.refresh_gate.lock().unwrap() = Some(gate.clone());
        let task = tokio::spawn(async move {
            routes
                .handle(
                    "mcp_server_status",
                    &json(r#"{"projectKey":" p ","sessionKey":"s"}"#),
                )
                .unwrap()
                .await
        });
        tokio::time::timeout(
            std::time::Duration::from_secs(3),
            project.refresh_started.notified(),
        )
        .await
        .expect("refresh must start");
        assert!(!task.is_finished());
        assert!(
            !project
                .calls
                .lock()
                .unwrap()
                .iter()
                .any(|call| call == "session_plugins")
        );
        gate.notify_one();
        assert_eq!(
            task.await.unwrap().unwrap(),
            json(r#"{"servers":[{"name":"plugin","status":"configured"}]}"#)
        );
        let calls = project.calls.lock().unwrap();
        assert_eq!(
            calls[0],
            format!("resolve:{:?}", Some(JsString::from(" p ")))
        );
        assert_eq!(&calls[1..], ["refresh", "refreshed", "session_plugins"]);
    }

    #[tokio::test]
    async fn status_propagates_input_and_refresh_errors_without_claiming_other_methods() {
        let (_, routes, project) = setup();
        assert!(!McpRoutes::handles("unknown"));
        assert!(routes.handle("unknown", &JsValue::Null).is_none());
        for (params, kind) in [(JsValue::Null, "null"), (JsValue::Undefined, "undefined")] {
            let error = routes
                .handle("mcp_server_status", &params)
                .unwrap()
                .await
                .unwrap_err();
            assert_eq!(error.name(), "TypeError");
            assert_eq!(error.code(), None);
            assert_eq!(
                error.message(),
                &JsString::from(format!(
                    "Cannot read properties of {kind} (reading 'projectKey')"
                ))
            );
        }
        assert!(project.calls.lock().unwrap().is_empty());
        project.fail.store(true, Ordering::SeqCst);
        let error = routes
            .handle("mcp_server_status", &json(r#"{"sessionKey":"s"}"#))
            .unwrap()
            .await
            .unwrap_err();
        assert_eq!(error.message(), &JsString::from("refresh failed"));
        assert_eq!(project.calls.lock().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn status_rejects_invalid_project_keys_before_resolving() {
        let (_, routes, project) = setup();
        for (value, received) in [
            ("42", "type number (42)"),
            ("false", "type boolean (false)"),
            ("[]", "an instance of Array"),
            ("{}", "an instance of Object"),
            (
                r#"{"constructor":{"name":{}}}"#,
                "an instance of [object Object]",
            ),
        ] {
            let params = json(&format!(r#"{{"projectKey":{value}}}"#));
            let error = routes
                .handle("mcp_server_status", &params)
                .unwrap()
                .await
                .unwrap_err();
            assert_eq!(error.code(), Some("ERR_INVALID_ARG_TYPE"));
            assert_eq!(
                error.message(),
                &JsString::from(format!(
                    r#"The "paths[0]" argument must be of type string. Received {received}"#
                ))
            );
        }
        let error = routes
            .handle(
                "mcp_server_status",
                &json(r#"{"projectKey":{"constructor":{"name":"\ud800"}}}"#),
            )
            .unwrap()
            .await
            .unwrap_err();
        assert_eq!(error.message().units().last(), Some(&0xd800));
        assert!(project.calls.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn native_assembly_refreshes_isolated_project_and_uses_fallback_root() {
        let scratch = crate::scratch::Scratch::new("mcp-routes");
        let routes = McpRoutes::native(scratch.path().join("home"), scratch.path().into());
        assert_eq!(
            routes
                .handle("mcp_server_status", &json("{}"))
                .unwrap()
                .await
                .unwrap(),
            json(r#"{"servers":[]}"#)
        );
        assert!(!scratch.path().join("home").exists());
    }
}
