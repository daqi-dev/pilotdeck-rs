//! A6.9-pr3e: CustomRouterRegistry from plugin contributions.
//! ProjectRuntimeResourcesBundle.ts:226-228 (createNativeRouterSessionCustomRouterPort).
use super::*;
use crate::runtime::{
    model::router::{CustomRouter, CustomRouterRegistry},
    plugins::PluginRuntime,
};
use pilotdeck_protocol::JsString;
use std::{any::Any, sync::Arc};

/// Project-owned custom router registry backed by plugin contributions.
/// Programmatic router lookup is not implemented yet; returns None.
pub struct ProjectCustomRouterRegistry {
    _plugins: Arc<PluginRuntime>,
}

impl ProjectCustomRouterRegistry {
    pub fn new(plugins: Arc<PluginRuntime>) -> Self {
        Self { _plugins: plugins }
    }
}

impl CustomRouterRegistry for ProjectCustomRouterRegistry {
    fn lookup_router(
        &self,
        _extension_id: &JsString,
        _session_id: &JsString,
    ) -> Option<Arc<dyn CustomRouter>> {
        // Programmatic router contributions are not implemented yet.
        // PluginRouterContribution exists but collect_router_contributions
        // returns an empty Vec. When programmatic routers are implemented,
        // this will query the plugin runtime's session contributions.
        None
    }
}

pub struct CustomRouterResources {
    pub registry: Arc<dyn CustomRouterRegistry>,
}

impl FamilyResource for CustomRouterResources {
    fn dispose(&self) -> DisposeFuture<'_> {
        Box::pin(async { Ok(()) })
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

pub struct CustomRouterFamilySource;

impl ResourceFamilySource for CustomRouterFamilySource {
    fn family(&self) -> Family {
        Family::RouterCustomRouters
    }

    fn stage(&self, context: &StageContext, sink: &mut StageSink<'_>) -> RuntimeResult {
        let plugin_resource = context
            .resource(Resource::PluginRuntime)
            .ok_or_else(|| RuntimeError::new("Plugin runtime is not staged."))?;
        let plugin_resource = plugin_resource
            .as_any()
            .downcast_ref::<super::plugins::PluginResources>()
            .ok_or_else(|| RuntimeError::new("Plugin resource is not a native plugin family."))?;
        let registry = Arc::new(ProjectCustomRouterRegistry::new(
            plugin_resource.runtime.clone(),
        ));
        let resource = Arc::new(CustomRouterResources {
            registry: registry as Arc<dyn CustomRouterRegistry>,
        });
        sink.register_owner(self.family(), resource.clone());
        sink.register_resource(Resource::RouterCustomRouters, resource);
        Ok(())
    }
}
