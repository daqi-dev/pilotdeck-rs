//! A6.9-pr3c1: MCP and execution-world families for ProjectResourcesFactory.
//! ProjectMcpRuntimeBundle.ts (mcp/runtime/ProjectMcpRuntimeProvider.ts:
//! options assembly) and ProjectExecutionWorldBundle.ts.
use super::*;
use crate::runtime::{
    mcp::{
        registry::{BuiltinMcpPaths, ProjectMcpRuntimeProvider, ProjectMcpRuntimeProviderOptions},
        sdk::SystemEnvironment,
    },
    model::http::ProxySettings,
};
use pilotdeck_protocol::JsString;
use std::{any::Any, sync::Arc};

// ---------------------------------------------------------------------------
// MCP family
// ---------------------------------------------------------------------------

/// Wraps a `ProjectMcpRuntimeProvider` as a `FamilyResource` so the
/// production staging machinery can own and dispose it.
struct McpProviderResource {
    provider: Arc<ProjectMcpRuntimeProvider>,
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
#[derive(Default)]
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
            builtin_paths: BuiltinMcpPaths::default(),
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
#[derive(Default)]
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

// Make the `JsString` → UTF-8 conversion available in this module.
trait JsStringExt {
    fn to_utf8(&self) -> Result<std::borrow::Cow<'_, str>, ()>;
}
impl JsStringExt for JsString {
    fn to_utf8(&self) -> Result<std::borrow::Cow<'_, str>, ()> {
        String::from_utf16(self.units())
            .map(std::borrow::Cow::Owned)
            .map_err(|_| ())
    }
}
