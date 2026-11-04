use super::{Options, assembly::Recorders, methods};
use crate::{
    metadata::object,
    runtime::{
        connection::{Call, RequestError},
        interaction::GatewayInteractionCoordinator,
        mcp::gateway::McpGateway,
        refresh::RefreshRouter,
        replacement::ReplacementCoordinator,
        session::SessionOwner,
        turn::{
            coordinator::TurnEventCoordinator, gateway::GatewayRequests, publish::FileRegistry,
        },
    },
};
use pilotdeck_protocol::{JsString, JsValue};
use std::sync::Arc;

#[derive(Clone)]
pub struct Requests {
    pub options: Options,
    pub env: crate::config::Env,
    pub snapshot_enabled: bool,
    pub coordinator: Arc<TurnEventCoordinator>,
    pub runs: Arc<FileRegistry>,
    pub owner: SessionOwner,
    pub project: Arc<crate::runtime::project::ProjectRuntimeRegistry>,
    pub replacement: Arc<ReplacementCoordinator>,
    pub recorders: Recorders,
    pub interactions: Arc<GatewayInteractionCoordinator>,
    /// U6 MCP four-control gateway adapter (#1215/#1319). The four SDK MCP
    /// control methods (`set_mcp_servers`, `mcp_server_reconnect`,
    /// `mcp_server_toggle`, `set_mcp_permission_mode_override`) route here.
    pub mcp: Arc<McpGateway>,
}

#[cfg(test)]
impl Requests {
    /// A stub `McpGateway` for test helpers that do not exercise MCP methods.
    /// The stub's `SdkMcpConfig` is wired with the supplied `owner` so it
    /// shares the same session close port as the surrounding test.
    pub fn stub_mcp(owner: &SessionOwner) -> Arc<McpGateway> {
        use crate::{
            config::permission::GatewaySessionPermissionRuleSetRegistry,
            runtime::{
                mcp::{
                    gateway::McpGatewayOptions,
                    sdk::{
                        SdkMcpCloseSession, SdkMcpConfig, SdkMcpConfigOptions, SdkMcpError,
                        SdkMcpFuture, SdkMcpProject, SdkMcpProjectResolver, SdkMcpSessionPlugins,
                        SystemEnvironment,
                    },
                },
                sdk_session::SdkSessionProjects,
            },
        };
        struct NullResolver;
        impl SdkMcpProjectResolver for NullResolver {
            fn resolve<'a>(
                &'a self,
                _: Option<&'a pilotdeck_protocol::JsString>,
            ) -> SdkMcpFuture<'a, Result<Arc<dyn SdkMcpProject>, SdkMcpError>> {
                Box::pin(async {
                    Err(SdkMcpError::plain(
                        "Error",
                        "Stub resolver: no project configured.",
                    ))
                })
            }
        }
        struct NullPlugins;
        impl SdkMcpSessionPlugins for NullPlugins {
            fn plugins(&self, _: &JsValue) -> Vec<Arc<crate::runtime::plugins::LoadedPlugin>> {
                Vec::new()
            }
        }
        let close_owner = owner.clone();
        let close_session: SdkMcpCloseSession = Arc::new(move |session_key, _reason| {
            let key = match &session_key {
                JsValue::String(key) => key.clone(),
                _ => pilotdeck_protocol::JsString::from(""),
            };
            let owner = close_owner.clone();
            Box::pin(async move {
                owner
                    .close(&key)
                    .await
                    .map_err(|e| SdkMcpError::plain("Error", JsString::from(e.message)))
            })
        });
        let config = Arc::new(SdkMcpConfig::new(SdkMcpConfigOptions {
            resolver: Arc::new(NullResolver),
            close_session,
            session_plugins: Arc::new(NullPlugins),
            permission_rule_sets: Arc::new(GatewaySessionPermissionRuleSetRegistry::new()),
            environment: Arc::new(SystemEnvironment),
            projects: SdkSessionProjects::shared(),
        }));
        Arc::new(McpGateway::new(McpGatewayOptions {
            owner: owner.clone(),
            config,
        }))
    }
}
impl GatewayRequests for Requests {
    fn describe_server(&self) -> Call<'_, Result<JsValue, RequestError>> {
        Box::pin(async {
            Ok(object([
                ("mode", "in_process".into()),
                (
                    "sessionCount",
                    JsValue::Number(self.owner.cached_session_count() as f64),
                ),
                (
                    "capabilities",
                    JsValue::Array(
                        [
                            "run_get",
                            "run_events",
                            "run_reattach",
                            "project_files_list",
                            "project_file_read",
                            "model_catalog_list",
                            "session_model_get",
                            "supported_agents",
                            "snapshot_list",
                            "snapshot_get",
                            "snapshot_restore",
                            "manager_sessions",
                            "commands_list",
                            "usage_snapshot",
                            "model_usage_snapshot",
                            "mcp_server_status",
                            "rewind_files",
                            #[cfg(unix)]
                            "background_task_stop",
                            #[cfg(unix)]
                            "background_tasks",
                            "set_mcp_servers",
                            "mcp_server_reconnect",
                            "mcp_server_toggle",
                            "set_mcp_permission_mode_override",
                        ]
                        .into_iter()
                        .filter(|capability| {
                            self.snapshot_enabled
                                || !matches!(
                                    *capability,
                                    "snapshot_list" | "snapshot_get" | "snapshot_restore"
                                )
                        })
                        .map(JsValue::from)
                        .collect(),
                    ),
                ),
            ]))
        })
    }
    fn request(
        &self,
        method: JsString,
        params: JsValue,
    ) -> Call<'_, Result<JsValue, RequestError>> {
        Box::pin(async move {
            // createLocalGateway's archive callbacks use the configured fallback
            // project for nullish keys. Fill it before isolated admission, while
            // leaving Metadata's cwd lazy for explicit paths and other RPCs.
            let mut params = params;
            if let Ok(name) = method.to_utf8()
                && crate::native_archive::ARCHIVE_METHODS.contains(&name.as_str())
            {
                crate::native_archive::fill_default_project_key(&mut params, &self.options.project);
            }
            if let Ok(name) = method.to_utf8()
                && let Some(result) = methods::before_admission(self, &name, &params).await
            {
                return result;
            }
            if let Some(project) = params.get("projectKey").filter(|v| !v.is_undefined())
                && !project
                    .as_string()
                    .and_then(|s| s.to_utf8().ok())
                    .is_some_and(|p| self.options.serves_project(&p))
            {
                return Err(RequestError::new(
                    "The isolated native process serves only its configured project.",
                ));
            }
            let method = method
                .to_utf8()
                .map_err(|_| RequestError::new("Unknown native gateway method."))?;
            methods::request(self, method, params).await
        })
    }
}
