use super::{
    GatewaySessionPermissionRuleSetRegistry, JsString, JsValue, McpGateway, McpGatewayOptions,
    PluginProjectRuntime, PluginResources, ProjectResources, ProjectRuntimeRegistry, Resource,
    SdkMcpCloseSession, SdkMcpConfig, SdkMcpConfigOptions, SdkMcpError, SdkMcpFuture,
    SdkMcpProject, SdkMcpProjectResolver, SdkMcpSessionPlugins, SdkSessionProjects, SessionOwner,
    SystemEnvironment,
};
use std::sync::Arc;

// ── U6 MCP resolver / session-plugins for the native process (#1319) ────────

/// The native process serves exactly one project. The SDK MCP resolver resolves
/// any `projectKey` (or no key) to that project's plugin runtime. This mirrors
/// `ProjectRuntimeRegistry.resolve` without exposing the full SDK session layer.
struct NativeMcpResolver {
    project: Arc<crate::runtime::project::ProjectRuntimeRegistry>,
}
impl SdkMcpProjectResolver for NativeMcpResolver {
    fn resolve<'a>(
        &'a self,
        _project_key: Option<&'a JsString>,
    ) -> SdkMcpFuture<'a, Result<Arc<dyn SdkMcpProject>, SdkMcpError>> {
        let project = self.project.clone();
        Box::pin(async move {
            let lease = project
                .acquire(None)
                .map_err(|e| SdkMcpError::plain("Error", JsString::from(e.to_string())))?;
            let resources = lease
                .runtime()
                .resources()
                .get::<ProjectResources>()
                .ok_or_else(|| {
                    SdkMcpError::plain("Error", "Native project resources are missing.")
                })?;
            let plugin_res = resources
                .context()
                .resource(Resource::PluginRuntime)
                .ok_or_else(|| {
                    SdkMcpError::plain("Error", "Native plugin runtime resource is missing.")
                })?;
            let plugin_runtime = plugin_res
                .as_any()
                .downcast_ref::<PluginResources>()
                .ok_or_else(|| {
                    SdkMcpError::plain("Error", "Native plugin runtime has the wrong type.")
                })?
                .runtime
                .clone();
            let project_root = JsString::from(
                lease
                    .runtime()
                    .project_root()
                    .to_string_lossy()
                    .into_owned(),
            );
            let resolved: Arc<dyn SdkMcpProject> =
                Arc::new(PluginProjectRuntime::new(project_root, plugin_runtime));
            Ok(resolved)
        })
    }
}

/// The native process has no SDK session plugin sources (only project-level
/// plugins apply). This satisfies the `SdkMcpSessionPlugins` contract with an
/// empty list, matching the TS path where `sdkSessionPlugins` is never set.
struct NoSessionPlugins;
impl SdkMcpSessionPlugins for NoSessionPlugins {
    fn plugins(&self, _session_key: &JsValue) -> Vec<Arc<crate::runtime::plugins::LoadedPlugin>> {
        Vec::new()
    }
}

pub(super) fn build_mcp_gateway(
    owner: &SessionOwner,
    build_project: &Arc<ProjectRuntimeRegistry>,
    permission_rule_sets: &Arc<GatewaySessionPermissionRuleSetRegistry>,
    build_mcp_config: &Arc<std::sync::OnceLock<Arc<SdkMcpConfig>>>,
) -> Arc<McpGateway> {
    // U6 MCP four-control gateway (#1319): wire the #1215 component
    // into the native request router. The SdkMcpConfig owns the
    // registry state; the McpGateway owns validation, the busy gate,
    // and the post-success close. The close_session callback mirrors
    // the TS `closeSession` that the registry calls after its commit.
    let close_owner = owner.clone();
    let close_session: SdkMcpCloseSession = Arc::new(move |session_key, _reason| {
        let key = match &session_key {
            JsValue::String(key) => key.clone(),
            _ => JsString::from(""),
        };
        let owner = close_owner.clone();
        Box::pin(async move {
            owner
                .close(&key)
                .await
                .map_err(|error| SdkMcpError::plain("Error", JsString::from(error.message)))
        })
    });
    let mcp_config = Arc::new(SdkMcpConfig::new(SdkMcpConfigOptions {
        resolver: Arc::new(NativeMcpResolver {
            project: build_project.clone(),
        }),
        close_session,
        session_plugins: Arc::new(NoSessionPlugins),
        permission_rule_sets: permission_rule_sets.clone(),
        environment: Arc::new(SystemEnvironment),
        projects: SdkSessionProjects::shared(),
    }));
    // Publish the config so the prepare closure's sync_permission_rules
    // call (on each session lease acquire) can reach it.  The set
    // succeeds once; subsequent calls from a second start_with_cleanup
    // invocation are silently ignored because the reference is stable.
    let _ = build_mcp_config.set(mcp_config.clone());
    let mcp_gateway = Arc::new(McpGateway::new(McpGatewayOptions {
        owner: owner.clone(),
        config: mcp_config,
    }));
    let _ = &mcp_gateway;
    mcp_gateway
}
