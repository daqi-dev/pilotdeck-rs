use super::{Options, SessionOverrides, assembly::Recorders, methods};
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
    /// A8 U8 cron runtime (#1326). `None` is TS `options.cron` unconfigured
    /// (no `cron:` section in the config): the six names stay out of
    /// capabilities and every call answers the `requireCron` gate
    /// (InProcessGateway.ts:3016-3022).
    pub cron: Option<Arc<crate::runtime::cron::CronManager>>,
    /// U7 global settings gateway (#1320): the six settings methods route
    /// here through the #1305 registry. Production binds
    /// [`crate::native_process::services::native_settings`].
    pub settings: Arc<SettingsGateway>,
    /// #1864: the per-gateway session selection store the three selector
    /// methods (`set_permission_mode` / `set_session_thinking` /
    /// `set_output_style`) write and the session factory's prepare closure
    /// reads back. One instance shared with the factory.
    pub session_overrides: Arc<SessionOverrides>,
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
                            // #1457: the seven session-config methods are now
                            // served for real (methods/config.rs). The
                            // spelling follows contract.capabilities in the
                            // oracle verbatim; the block keeps the oracle's
                            // relative order (the production list itself is
                            // not oracle-ordered).
                            "session_model_set",
                            "session_model_clear",
                            "set_permission_mode",
                            "set_session_thinking",
                            "output_styles_list",
                            "set_output_style",
                            "seed_read_state",
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
                            // #1320: TS advertises all four settings methods in
                            // every config state (oracle.json `contract/`,
                            // `contract/emptyStorage/`, `contract/missingStorage/`
                            // capabilities: apply_flag_settings, update_settings,
                            // resolve_settings, reload_output_styles — never
                            // reload_config / reload_extensions). The native
                            // process serves only `resolve_settings` for real;
                            // the other three have no delegate here and always
                            // answer CAPABILITY_UNAVAILABLE, so advertising them
                            // would promise what cannot be delivered (the
                            // `assert_ts_capability_subset` guard allows the
                            // Rust set to be a subset of TS). Porting them is
                            // #1671; deriving the list per provider from a
                            // registry instead of this fixed list is #1352.
                            "resolve_settings",
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
                        // TS splices the six cron names right after the
                        // snapshot triplet, gated on `options.cron`
                        // (InProcessGateway.ts:2114-2122). The advertised set
                        // is order-insensitive on both sides — every Rust
                        // assertion sorts before comparing and the TS-subset
                        // guard is containment — so the names are appended to
                        // the same chain rather than splitting the literal
                        // (the shared #1316 negative controls anchor on that
                        // literal). `CRON_METHODS` stays the single name owner
                        // (#1325), so the advertisement, the capability filter
                        // and the family registry cannot drift apart.
                        .into_iter()
                        .chain(crate::runtime::cron::CRON_METHODS)
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
                                && (self.cron.is_some()
                                    || !crate::runtime::cron::CRON_METHODS.contains(capability))
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
            // #2083 r1: this admission gate is the same source as the
            // handler's configured-project check (`session_close_project`),
            // so the key converts lossily here too. A lone-surrogate
            // `projectKey` is refused as any other unserved key, and the
            // registered residual — a lossy form naming a configured path
            // that contains U+FFFD — is admitted on both layers alike.
            if let Some(project) = params.get("projectKey").filter(|v| !v.is_undefined())
                && !project.as_string().is_some_and(|key| {
                    let lossy = String::from_utf16_lossy(key.units());
                    self.options.serves_project(&lossy)
                })
            {
                return Err(RequestError::new(
                    "The isolated native process serves only its configured project.",
                ));
            }
            let method = method
                .to_utf8()
                .map_err(|_| RequestError::new("Unknown native gateway method."))?;
            if [
                "active_turn_snapshot",
                "read_session_messages",
                "permission_decide",
                "reconnect_interaction",
                "export_session_transcript",
                "restore_session_transcript",
            ]
            .contains(&method.as_str())
                && let Some(session) = params.get("sessionKey").and_then(JsValue::as_string)
            {
                let handle = if let Some(handle) = self.owner.current_handle(session) {
                    Some(handle)
                } else {
                    let mut path =
                        super::assembly::transcript_path(&self.options, session).into_os_string();
                    path.push(".backend.json");
                    let kernel_owned = std::fs::read(std::path::PathBuf::from(path))
                        .ok()
                        .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
                        .is_some_and(|owner| owner["backend"] == "kernel");
                    if kernel_owned {
                        Some(
                            self.owner
                                .get_or_create(crate::runtime::session::SessionContext {
                                    session_key: session.clone(),
                                    project_key: Some(
                                        self.options.project.to_string_lossy().into_owned().into(),
                                    ),
                                    channel_key: "gateway".into(),
                                    allowed_tools: None,
                                    disallowed_tools: None,
                                })
                                .await
                                .map_err(|e| RequestError::new(e.message))?,
                        )
                    } else {
                        None
                    }
                };
                if let Some(handle) = &handle
                    && handle.backend_kind() == "kernel"
                {
                    if method == "permission_decide" {
                        let received = params.get("interactionBinding").and_then(|v| {
                            crate::runtime::interaction::InteractionBinding::from_value(v).ok()
                        });
                        if received != self.interactions.get_binding(session) {
                            return Ok(object([("delivered", JsValue::Bool(false))]));
                        }
                    }
                    if method == "reconnect_interaction" {
                        let next = params
                            .get("nextBinding")
                            .map(crate::runtime::interaction::InteractionBinding::from_value)
                            .transpose()
                            .map_err(RequestError::new)?;
                        let previous = params.get("previousBinding").and_then(|v| {
                            crate::runtime::interaction::InteractionBinding::from_value(v).ok()
                        });
                        let gate = self
                            .interactions
                            .reconnect(session, next, previous.as_ref())
                            .map_err(RequestError::new)?;
                        if gate
                            .get("outcome")
                            .and_then(JsValue::as_string)
                            .is_some_and(|v| v.equals_str("stale_binding"))
                        {
                            return Ok(gate);
                        }
                    }
                }
                if let Some(handle) = handle
                    && let Some(mut value) = handle
                        .backend_request(method.clone(), params.clone())
                        .await
                        .map_err(|e| RequestError::new(e.message))?
                {
                    if method == "active_turn_snapshot" {
                        let mut projection = self
                            .coordinator
                            .snapshot(&crate::runtime::turn::replay::ActiveTurnSnapshotInput::new(
                                session.clone(),
                            ))
                            .to_json();
                        if let JsValue::Object(ref mut p) = projection {
                            p.insert("nativeExecution".into(), value);
                        }
                        value = projection;
                    } else if method == "read_session_messages" {
                        if let JsValue::Object(ref mut history) = value {
                            history.insert(
                                "stream".into(),
                                self.coordinator
                                    .snapshot(
                                        &crate::runtime::turn::replay::ActiveTurnSnapshotInput::new(
                                            session.clone(),
                                        ),
                                    )
                                    .to_json(),
                            );
                        }
                    }
                    return Ok(value);
                }
            }
            methods::request(self, method, params).await
        })
    }
}
