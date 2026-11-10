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
use std::collections::HashSet;
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
    /// A7.8 always-on background runtime (#2294): the automation bundle's
    /// staged always-on manager, the port of TS `options.alwaysOnControl`.
    /// `None` is the unconfigured shape (no `alwaysOn:` section, or no
    /// project with `enabled: true`): the three names stay out of
    /// capabilities and every call answers the #1313 `not_configured`
    /// envelopes byte for byte (InProcessGateway.ts:2992-3016).
    pub always_on: Option<Arc<crate::runtime::always_on::AlwaysOnManager>>,
    /// U7 global settings gateway (#1320): the six settings methods route
    /// here through the #1305 registry. Production binds
    /// [`crate::native_process::services::native_settings`].
    pub settings: Arc<SettingsGateway>,
    /// #1864: the per-gateway session selection store the three selector
    /// methods (`set_permission_mode` / `set_session_thinking` /
    /// `set_output_style`) write and the session factory's prepare closure
    /// reads back. One instance shared with the factory.
    pub session_overrides: Arc<SessionOverrides>,
    /// #2622: the TS gateway's `restoringSessionKeys` set
    /// (createLocalGateway.ts:1306-1307). `restore_session_transcript` reads
    /// membership where TS does (before the storage work) and rides its
    /// add/remove pair inside the fenced blocking write, so a cancelled
    /// request cannot strand a key.
    pub restoring_session_keys: Arc<Mutex<HashSet<JsString>>>,
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
            Ok(object([
                ("mode", "in_process".into()),
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
            // #2272: the filter skips `undefined` *and* `null` — TS reads
            // `input.projectKey ?? fallback` (createLocalGateway.ts), so an
            // explicit null is "not provided" and takes the handler-side
            // configured-project fallback like `resolve_project` does, not
            // the unserved-key refusal.
            if let Some(project) = params
                .get("projectKey")
                .filter(|v| !matches!(v, JsValue::Undefined | JsValue::Null))
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
            methods::request(self, method, params).await
        })
    }
}
