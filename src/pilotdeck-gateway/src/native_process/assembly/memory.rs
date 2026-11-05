use super::{
    JsString, MemoryGateway, MemoryManagementResource, MemoryManagementService, MemoryProjection,
    MemoryProviderError, Options, ProjectMemoryRegistry, ProjectResources, ProjectRuntimeRegistry,
    ProxyError, Resource, RuntimeError,
};
use std::sync::Arc;

/// `ProjectMemoryRegistry` adapter over the native `ProjectRuntimeRegistry`.
/// Mirrors the single-project `dialog.projects` surface from
/// `createLocalGateway.ts:1238-1259`: `resolveProjectKey` accepts the
/// configured project path and returns it unchanged (native process serves
/// only one project); any other key is rejected with `PROJECT_NOT_FOUND`.
/// `memory_management` acquires the current generation and reads
/// `Resource::MemoryManagement` from the staged context, returning `None`
/// when memory management was not staged (provider disabled or absent).
struct NativeMemoryRegistry {
    project: Arc<ProjectRuntimeRegistry>,
    project_root: std::path::PathBuf,
}

impl ProjectMemoryRegistry for NativeMemoryRegistry {
    fn resolve_project_key(&self, project_key: &JsString) -> Result<JsString, MemoryProviderError> {
        let root = self
            .project_root
            .to_str()
            .ok_or_else(MemoryProviderError::host_required)?;
        if project_key.equals_str(root) {
            Ok(project_key.clone())
        } else {
            Err(MemoryProviderError::unknown_project_key(
                project_key.units(),
            ))
        }
    }

    fn memory_management(
        &self,
        _project_key: &JsString,
    ) -> Result<Option<Arc<dyn MemoryManagementService>>, MemoryProviderError> {
        let lease = self
            .project
            .acquire(None)
            .map_err(MemoryProviderError::from)?;
        let resources = lease
            .runtime()
            .resources()
            .get::<ProjectResources>()
            .ok_or_else(|| MemoryProviderError::new("Native project resources are missing."))?;
        let context = resources.context();
        let Some(handle) = context.resource(Resource::MemoryManagement) else {
            return Ok(None);
        };
        let resource = handle
            .as_any()
            .downcast_ref::<MemoryManagementResource>()
            .ok_or_else(|| {
                MemoryProviderError::new("Native memory management resource has the wrong type.")
            })?;
        Ok(Some(resource.management().clone()))
    }
}

/// Build the `MemoryGateway` by probing the current project generation. If
/// `Resource::MemoryManagement` is staged the gateway is enabled; otherwise
/// (`MemoryGateway::disabled()`) it advertises `CAPABILITY_UNAVAILABLE` after
/// parameter validation, matching the TS wrapper seam.
fn build_memory_gateway(
    options: &Options,
    project: &Arc<ProjectRuntimeRegistry>,
) -> Result<MemoryGateway, RuntimeError> {
    let registry = Arc::new(NativeMemoryRegistry {
        project: project.clone(),
        project_root: options.project.clone(),
    });
    // Check if the current generation staged a management resource. If the
    // project has no memory provider the gateway is disabled but not an error.
    let lease = project.acquire(None)?;
    let resources = lease
        .runtime()
        .resources()
        .get::<ProjectResources>()
        .ok_or_else(|| RuntimeError::new("Native project resources are missing."))?;
    let context = resources.context();
    if context.resource(Resource::MemoryManagement).is_some() {
        let projection = MemoryProjection::new(registry, options.project.clone());
        Ok(MemoryGateway::from_projection(projection))
    } else {
        Ok(MemoryGateway::disabled())
    }
}

pub(super) async fn build_memory(
    options: Options,
    project: Arc<ProjectRuntimeRegistry>,
) -> Result<Arc<MemoryGateway>, ProxyError> {
    let memory = match build_memory_gateway(&options, &project) {
        Ok(gateway) => Arc::new(gateway),
        Err(error) => {
            let _ = tokio::time::timeout(options.drain_budget, project.dispose()).await;
            return Err(error.into());
        }
    };
    Ok(memory)
}
