//! A6.9-pr3c1: MCP and execution-world families for ProjectResourcesFactory.
//! ProjectMcpRuntimeBundle.ts (mcp/runtime/ProjectMcpRuntimeProvider.ts:
//! options assembly) and ProjectExecutionWorldBundle.ts.
use super::*;
use crate::runtime::{
    mcp::{
        registry::{ProjectMcpRuntimeProvider, ProjectMcpRuntimeProviderOptions},
        sdk::SystemEnvironment,
    },
    model::http::ProxySettings,
};
use std::{any::Any, sync::Arc};

// ---------------------------------------------------------------------------
// MCP family
// ---------------------------------------------------------------------------

/// Wraps a `ProjectMcpRuntimeProvider` as a `FamilyResource` so the
/// production staging machinery can own and dispose it.
pub struct McpProviderResource {
    provider: Arc<ProjectMcpRuntimeProvider>,
}
impl McpProviderResource {
    /// #1433: the session prepare closure reads the provider to compose each
    /// session's MCP tools; the resource stays the staging owner.
    pub fn provider(&self) -> &Arc<ProjectMcpRuntimeProvider> {
        &self.provider
    }
}
impl FamilyResource for McpProviderResource {
    fn dispose(&self) -> DisposeFuture<'_> {
        Box::pin(self.provider.dispose())
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// Production MCP family source.  Constructs a `ProjectMcpRuntimeProvider`
/// from the staged config and registers it as the McpProvider owner.
pub struct McpFamilySource;

impl ResourceFamilySource for McpFamilySource {
    fn family(&self) -> Family {
        Family::McpProvider
    }

    fn stage(&self, context: &StageContext, sink: &mut StageSink<'_>) -> RuntimeResult {
        let project_root = context.project_root();
        let pilot_home = context.pilot_home();
        let snapshot = context.snapshot();

        let proxy_settings = snapshot
            .get("config")
            .and_then(|c| c.get("proxy"))
            .and_then(|v| v.get("url"))
            .and_then(|u| u.as_string())
            .filter(|url| !url.is_empty())
            .map(|url| ProxySettings {
                url: String::from_utf16_lossy(url.units()),
                no_proxy: None,
                ..ProxySettings::default()
            });

        // SDK discovery must not make the project generation depend on a live
        // process cwd. Installed roots and executable siblings remain usable
        // after cwd deletion; session composition owns the TS uv_cwd error.
        let sdk_cwd = std::env::current_dir().unwrap_or_else(|_| project_root.to_path_buf());
        let options = ProjectMcpRuntimeProviderOptions {
            project_root: project_root
                .to_str()
                .ok_or_else(|| RuntimeError::new("MCP: project root is not valid UTF-8"))?
                .to_owned(),
            pilot_home: pilot_home
                .to_str()
                .ok_or_else(|| RuntimeError::new("MCP: pilot home is not valid UTF-8"))?
                .to_owned(),
            create_runtime: None,
            proxy: proxy_settings,
            on_diagnostic: None,
            environment: Arc::new(SystemEnvironment),
            builtin_paths: crate::runtime::mcp::native_builtins::builtin_paths(
                &context.env(),
                &std::env::current_exe().map_err(|e| RuntimeError::new(e.to_string()))?,
                &sdk_cwd,
            )
            .map_err(|e| RuntimeError::new(e.to_string()))?,
        };

        let provider = Arc::new(ProjectMcpRuntimeProvider::new(options));
        let resource = Arc::new(McpProviderResource {
            provider: provider.clone(),
        });

        sink.register_owner(self.family(), resource);
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// ExecutionWorld family (minimal — exposes sandbox/fs/network/shell to tools;
// full wiring is A6.9-pr3c2 scope)
// ---------------------------------------------------------------------------

struct ExecutionWorldResource;
impl FamilyResource for ExecutionWorldResource {
    fn dispose(&self) -> DisposeFuture<'_> {
        Box::pin(async { Ok(()) })
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// Minimal production execution-world family source.  Registers the bundle
/// owner so downstream staging does not fail; full sandbox wiring follows in
/// A6.9-pr3c2.
pub struct ExecutionWorldFamilySource;

impl ResourceFamilySource for ExecutionWorldFamilySource {
    fn family(&self) -> Family {
        Family::ExecutionWorld
    }

    fn stage(&self, _context: &StageContext, sink: &mut StageSink<'_>) -> RuntimeResult {
        let resource = Arc::new(ExecutionWorldResource);
        sink.register_owner(self.family(), resource.clone());
        sink.register_resource(Resource::ExecutionWorld, resource);
        Ok(())
    }
}
