use super::{Options, assembly::Recorders, methods};
use crate::{
    metadata::object,
    runtime::{
        async_hooks::AsyncHookRegistry,
        connection::{Call, RequestError},
        interaction::GatewayInteractionCoordinator,
        mcp::gateway::McpGateway,
        memory::gateway::MemoryGateway,
        refresh::RefreshRouter,
        replacement::ReplacementCoordinator,
        session::SessionOwner,
        settings::SettingsGateway,
        turn::{
            coordinator::TurnEventCoordinator, gateway::GatewayRequests, publish::FileRegistry,
        },
    },
    upload::gateway::UploadGateway,
    user_dialog::GatewayUserDialogBus,
};
use pilotdeck_protocol::{JsString, JsValue};
use std::sync::{Arc, Mutex};

#[derive(Clone)]
pub struct Requests {
    pub options: Options,
    pub env: crate::config::Env,
    pub snapshot_enabled: bool,
    pub memory: Arc<MemoryGateway>,
    pub coordinator: Arc<TurnEventCoordinator>,
    pub runs: Option<Arc<FileRegistry>>,
    pub owner: SessionOwner,
    pub project: Arc<crate::runtime::project::ProjectRuntimeRegistry>,
    pub replacement: Arc<ReplacementCoordinator>,
    pub recorders: Recorders,
    pub interactions: Arc<GatewayInteractionCoordinator>,
    /// U6 MCP four-control gateway adapter (#1215/#1319). The four SDK MCP
    /// control methods (`set_mcp_servers`, `mcp_server_reconnect`,
    /// `mcp_server_toggle`, `set_mcp_permission_mode_override`) route here.
    pub mcp: Arc<McpGateway>,
    /// Gateway-level async hook registry for `hook_async_result`.
    /// `None` when the project has no `InteractionSteer` resource (no steer wired).
    pub async_hook_registry: Option<Arc<AsyncHookRegistry>>,
    /// Gateway user-dialog bus backing the `user_dialog` / `respond` methods.
    pub user_dialog_bus: Arc<Mutex<GatewayUserDialogBus>>,
    /// A8 U1 upload + attachment lease gateway (#1321).
    pub upload_gateway: Arc<UploadGateway>,
    /// U12 `prepareWeixinLogin` callback (InProcessGateway options field,
    /// set through `setPrepareWeixinLogin`). `None` is the TS
    /// "callback not configured" shape: `{requested: false, reason:
    /// "unsupported"}` with a fresh `requestedAt`.
    pub prepare_weixin_login: Option<methods::PrepareWeixinLogin>,
    /// U7 global settings gateway (#1320): the six settings methods route
    /// here through the #1305 registry. Production binds
    /// [`crate::native_process::services::native_settings`].
    pub settings: Arc<SettingsGateway>,
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

    /// A `SettingsGateway` for test fixtures that do not exercise the U7
    /// settings family: the real #1318 component with no delegate bound, so
    /// the four capability-gated methods reject `CAPABILITY_UNAVAILABLE` and
    /// the two reloads answer `{reloaded:false, reason:"unsupported"}` — the
    /// TS unconfigured projections, produced by the component itself.
    pub fn stub_settings(owner: &SessionOwner) -> Arc<SettingsGateway> {
        use crate::runtime::settings::SettingsGatewayOptions;
        Arc::new(SettingsGateway::new(SettingsGatewayOptions {
            owner: owner.clone(),
            apply_flag_settings: None,
            update_settings: None,
            resolve_settings: None,
            reload_output_styles: None,
            reload_config: None,
            reload_extensions: None,
        }))
    }
}
impl GatewayRequests for Requests {
    fn describe_server(&self) -> Call<'_, Result<JsValue, RequestError>> {
        Box::pin(async {
            let memory_enabled = self.memory.is_enabled();
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
                            "memory_list",
                            "memory_wipe",
                            // TS advertises both in every config state
                            // (oracle.json:2801-2802, 2855-2856, 2956-2957).
                            // permission_list is fully wired; user_dialog_list
                            // is wired too. Production builds a connected bus
                            // (#1445), so runtime availability is not gated on
                            // a renderer change observer. Forwarding
                            // `user_dialog_changed` remains a separate
                            // broadcast gap.
                            "user_dialog_list",
                            "permission_list",
                            // TS advertises these three unconditionally
                            // (oracle.json capabilities); the native process
                            // now serves them for real: the interaction
                            // round-trip family (#1316), the steer family's
                            // hook_async_result, and session deletion.
                            "permission_decide",
                            "async_hook_result",
                            "delete_session",
                        ]
                        .into_iter()
                        .chain([
                            "upload_create",
                            "upload_get",
                            "upload_part",
                            "upload_complete",
                            "upload_cancel",
                        ])
                        .filter(|capability| {
                            self.runs.is_some()
                                || !matches!(*capability, "run_get" | "run_events" | "run_reattach")
                        })
                        .filter(|capability| {
                            (self.snapshot_enabled
                                || !matches!(
                                    *capability,
                                    "snapshot_list" | "snapshot_get" | "snapshot_restore"
                                ))
                                && (memory_enabled
                                    || !matches!(*capability, "memory_list" | "memory_wipe"))
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
