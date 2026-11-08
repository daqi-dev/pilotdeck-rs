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
impl Requests {
    pub(in crate::native_process) fn kernel_owned(
        &self,
        session: &JsString,
    ) -> Result<bool, RequestError> {
        if let Some(handle) = self.owner.current_handle(session) {
            return Ok(handle.backend_kind() == "kernel");
        }
        let transcript = super::assembly::transcript_path(&self.options, session);
        let mut path = transcript.as_os_str().to_os_string();
        path.push(".backend.json");
        match crate::runtime::session::kernel::persisted_owner(&std::path::PathBuf::from(path))
            .map_err(RequestError::from_runtime)?
        {
            Some(owner) => Ok(owner["backend"] == "kernel"),
            None => Ok(!std::fs::metadata(transcript).is_ok_and(|m| m.len() > 0)
                && self
                    .env
                    .get("PILOTDECK_SESSION_BACKEND")
                    .map(String::as_str)
                    == Some("kernel")),
        }
    }
}
impl GatewayRequests for Requests {
    fn describe_server(&self) -> Call<'_, Result<JsValue, RequestError>> {
        Box::pin(async {
            Ok(object([
                ("mode", "in_process".into()),
                (
                    "sessionBackendDefault",
                    self.env
                        .get("PILOTDECK_SESSION_BACKEND")
                        .map(String::as_str)
                        .unwrap_or("legacy")
                        .into(),
                ),
                ("kernelProfile", super::kernel_profile::description()),
                (
                    "sessionCount",
                    JsValue::Number(self.owner.cached_session_count() as f64),
                ),
                (
                    "capabilities",
                    // One table, composed per provider: `capabilities::advertised`
                    // drops a name when its provider is not bound here or when a
                    // method it promises is missing from the #1305 registry.
                    // `hello_ok` reports this same call as `serverInfo`
                    // (runtime/connection/mod.rs), so the two cannot drift.
                    JsValue::Array(
                        super::capabilities::advertised(self)
                            .into_iter()
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
            if super::kernel_profile::BOUND_CONFIG_METHODS.contains(&method.as_str())
                && let Some(session) = params.get("sessionKey").and_then(JsValue::as_string)
                && self.kernel_owned(session)?
            {
                let handle = if let Some(handle) = self.owner.current_handle(session) {
                    handle
                } else {
                    self.owner
                        .get_or_create(crate::runtime::session::SessionContext {
                            session_key: session.clone(),
                            project_key: Some(
                                self.options.project.to_string_lossy().into_owned().into(),
                            ),
                            channel_key: "configuration".into(),
                            allowed_tools: None,
                            disallowed_tools: None,
                        })
                        .await
                        .map_err(RequestError::from_runtime)?
                };
                let snapshot = handle
                    .backend_request("active_turn_snapshot".into(), params.clone())
                    .await
                    .map_err(RequestError::from_runtime)?
                    .ok_or_else(|| RequestError::new("Kernel state is unavailable."))?;
                if snapshot
                    .get("status")
                    .and_then(JsValue::as_string)
                    .is_some_and(|state| {
                        ["accepted", "running", "needs_human"]
                            .iter()
                            .any(|expected| state.equals_str(expected))
                    })
                {
                    return Err(RequestError{code:"SESSION_BUSY".into(),message:"Cannot change native turn preferences while the original Kernel execution is active.".into(),details:Some(Box::new(snapshot)),validation:None});
                }
            }
            if super::kernel_profile::UNSUPPORTED_SESSION_METHODS.contains(&method.as_str()) {
                let kernel = match params.get("sessionKey").and_then(JsValue::as_string) {
                    Some(session) => self.kernel_owned(session)?,
                    None => {
                        self.env
                            .get("PILOTDECK_SESSION_BACKEND")
                            .map(String::as_str)
                            == Some("kernel")
                    }
                };
                if kernel {
                    return Err(RequestError {code:"CAPABILITY_UNAVAILABLE".into(),message:format!("Kernel session profile does not support {method}; its assembly/configuration is pinned. No legacy storage or runtime was changed.").into(),details:Some(Box::new(super::kernel_profile::description())),validation:None});
                }
            }
            if super::kernel_profile::BACKEND_METHODS.contains(&method.as_str())
                && let Some(session) = params.get("sessionKey").and_then(JsValue::as_string)
            {
                let handle = if let Some(handle) = self.owner.current_handle(session) {
                    Some(handle)
                } else {
                    let mut path =
                        super::assembly::transcript_path(&self.options, session).into_os_string();
                    path.push(".backend.json");
                    let kernel_owned = crate::runtime::session::kernel::persisted_owner(
                        &std::path::PathBuf::from(path),
                    )
                    .map_err(RequestError::from_runtime)?
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
                                .map_err(RequestError::from_runtime)?,
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
                            .reconnect(
                                session,
                                next,
                                if self
                                    .interactions
                                    .reconnect_port()
                                    .has_binding_history(session)
                                {
                                    previous.as_ref()
                                } else {
                                    None
                                },
                            )
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
                        .map_err(RequestError::from_runtime)?
                {
                    if method == "reconnect_interaction" {
                        if let JsValue::Object(ref mut output) = value {
                            output.insert(
                                "binding".into(),
                                self.interactions
                                    .get_binding(session)
                                    .map(|binding| binding.to_value())
                                    .unwrap_or(JsValue::Null),
                            );
                        }
                    }
                    if method == "run_get" || method == "run_reattach" || method == "run_events" {
                        let native = value;
                        let mut projected =
                            methods::request(self, method.clone(), params.clone()).await?;
                        if let JsValue::Object(ref mut p) = projected {
                            p.insert("nativeExecution".into(), native.clone());
                            if method != "run_events" {
                                if let Some(state) = native
                                    .get("state")
                                    .and_then(JsValue::as_string)
                                    .and_then(|s| s.to_utf8().ok())
                                {
                                    let state = match state.as_str() {
                                        "needs_human" => "running",
                                        "cancelled" => "aborted",
                                        "unknown" => "interrupted",
                                        other => other,
                                    };
                                    p.insert("state".into(), state.into());
                                }
                            }
                        }
                        value = projected;
                    } else if method == "active_turn_snapshot" {
                        if value.get("attached") != Some(&JsValue::Bool(true)) {
                            return crate::runtime::session::kernel_replay::snapshot(
                                &value,
                                &String::from_utf16_lossy(session.units()),
                                &self.options.home,
                            )
                            .map_err(|e| RequestError::new(e.message));
                        }
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
                        let native = handle
                            .backend_request("active_turn_snapshot".into(), params.clone())
                            .await
                            .map_err(RequestError::from_runtime)?
                            .ok_or_else(|| {
                                RequestError::new("Native active state is unavailable.")
                            })?;
                        let stream = if native.get("attached") == Some(&JsValue::Bool(true)) {
                            self.coordinator
                                .snapshot(
                                    &crate::runtime::turn::replay::ActiveTurnSnapshotInput::new(
                                        session.clone(),
                                    ),
                                )
                                .to_json()
                        } else {
                            crate::runtime::session::kernel_replay::snapshot(
                                &native,
                                &String::from_utf16_lossy(session.units()),
                                &self.options.home,
                            )
                            .map_err(RequestError::from_runtime)?
                        };
                        if let JsValue::Object(ref mut history) = value {
                            history.insert("stream".into(), stream);
                        }
                    }
                    return Ok(value);
                }
            }
            methods::request(self, method, params).await
        })
    }
}
