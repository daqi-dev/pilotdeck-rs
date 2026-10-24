//! A6.9-pr3e: PluginRuntime family for ProjectRuntimeFactory.
//! ProjectRuntimeResourcesBundle.ts:218-225.
use super::*;
use crate::runtime::plugins::{PluginRuntime, PluginRuntimeOptions};
use std::{any::Any, sync::Arc};

pub struct PluginFamilySource {
    pub options: PluginRuntimeOptions,
}

pub struct PluginResources {
    pub runtime: Arc<PluginRuntime>,
}

impl FamilyResource for PluginResources {
    fn dispose(&self) -> DisposeFuture<'_> {
        Box::pin(async {
            self.runtime
                .dispose()
                .await
                .map_err(|error| RuntimeError::new(error.to_string()))
        })
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

impl ResourceFamilySource for PluginFamilySource {
    fn family(&self) -> Family {
        Family::PluginRuntime
    }

    fn stage(&self, _context: &StageContext, sink: &mut StageSink<'_>) -> RuntimeResult {
        let runtime = Arc::new(PluginRuntime::new(PluginRuntimeOptions {
            project_root: self.options.project_root.clone(),
            pilot_home: self.options.pilot_home.clone(),
            builtin_skills_root: self.options.builtin_skills_root.clone(),
            builtin_plugins: self.options.builtin_plugins.clone(),
            builtin_plugins_enabled: self.options.builtin_plugins_enabled.clone(),
        }));
        let resource = Arc::new(PluginResources { runtime });
        sink.register_owner(self.family(), resource.clone());
        sink.register_resource(Resource::PluginRuntime, resource);
        Ok(())
    }
}
