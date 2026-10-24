//! Memory bundle family: provider composition, maintenance and management
//! surfaces (TS ProjectMemoryBundle.ts:41-86).
//!
//! The bundle is registered as a dispose owner before its `stage()` can throw,
//! matching the baseline's field assignment order. Disposal awaits the
//! provider's own cleanup.

use super::{
    DisposeFuture, Family, FamilyResource, Resource, ResourceFamilySource, RuntimeError,
    RuntimeResult, StageContext, StageSink,
};
use crate::runtime::memory::{MemoryConfig, ProjectMemoryBundle, ProjectMemoryBundleOptions};
use std::any::Any;
use std::sync::Arc;

/// The MemoryBundle family source (ProjectMemoryBundle).
pub struct MemoryBundleFamilySource;

impl ResourceFamilySource for MemoryBundleFamilySource {
    fn family(&self) -> Family {
        Family::MemoryBundle
    }

    fn stage(&self, context: &StageContext, sink: &mut StageSink<'_>) -> RuntimeResult {
        let snapshot = context.snapshot();
        let project_root = context.project_root();

        // Parse memory config from snapshot
        let config = snapshot
            .get("config")
            .and_then(|config| config.get("memory"))
            .and_then(|memory| {
                let enabled = memory.get("enabled")?.as_bool()?;
                let provider = memory.get("provider")?.as_string()?.to_utf8().ok()?;
                Some(MemoryConfig { enabled, provider })
            });

        // Parse agent model from snapshot
        // config.agent.model is an object with "id" field, not a string
        let agent_model = snapshot
            .get("config")
            .and_then(|config| config.get("agent"))
            .and_then(|agent| agent.get("model"))
            .and_then(|model| model.get("id"))
            .and_then(|id| id.as_string())
            .and_then(|s| s.to_utf8().ok());

        let options = ProjectMemoryBundleOptions {
            project_root,
            config,
            agent_model,
            provider_factory: None,
            create_provider: None,
        };

        let bundle = Arc::new(ProjectMemoryBundle::new(options));

        // Register the bundle as dispose owner BEFORE staging (TS field assignment order)
        sink.register_owner(
            Family::MemoryBundle,
            Arc::new(MemoryBundleResource {
                bundle: Arc::clone(&bundle),
            }),
        );

        // Stage the bundle to get resources
        let resources = bundle.stage().map_err(|error| {
            RuntimeError::new(format!("Failed to stage memory bundle: {}", error))
        })?;

        // Register published resources
        if let Some(memory) = resources.memory {
            sink.register_resource(
                Resource::Memory,
                Arc::new(MemoryResolverResource { resolver: memory }),
            );
        }

        if let Some(maintenance) = resources.maintenance {
            sink.register_resource(
                Resource::MemoryService,
                Arc::new(MemoryMaintenanceResource { maintenance }),
            );
        }

        if let Some(management) = resources.management {
            sink.register_resource(
                Resource::MemoryManagement,
                Arc::new(MemoryManagementResource { management }),
            );
        }

        Ok(())
    }
}

/// FamilyResource wrapper for the ProjectMemoryBundle (dispose owner).
struct MemoryBundleResource {
    bundle: Arc<ProjectMemoryBundle>,
}

impl FamilyResource for MemoryBundleResource {
    fn dispose(&self) -> DisposeFuture<'_> {
        self.bundle.dispose()
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// FamilyResource wrapper for memory resolver.
struct MemoryResolverResource {
    resolver: Arc<dyn crate::runtime::context::memory::MemoryResolver>,
}

impl FamilyResource for MemoryResolverResource {
    fn dispose(&self) -> DisposeFuture<'_> {
        // Memory resources are disposed through the bundle
        Box::pin(async { Ok(()) })
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

#[allow(dead_code)]
impl MemoryResolverResource {
    pub fn resolver(&self) -> &Arc<dyn crate::runtime::context::memory::MemoryResolver> {
        &self.resolver
    }
}

/// FamilyResource wrapper for memory maintenance service.
struct MemoryMaintenanceResource {
    maintenance: Arc<dyn crate::runtime::memory::MemoryMaintenanceService>,
}

impl FamilyResource for MemoryMaintenanceResource {
    fn dispose(&self) -> DisposeFuture<'_> {
        Box::pin(async { Ok(()) })
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

#[allow(dead_code)]
impl MemoryMaintenanceResource {
    pub fn maintenance(&self) -> &Arc<dyn crate::runtime::memory::MemoryMaintenanceService> {
        &self.maintenance
    }
}

/// FamilyResource wrapper for memory management service.
struct MemoryManagementResource {
    management: Arc<dyn crate::runtime::memory::MemoryManagementService>,
}

impl FamilyResource for MemoryManagementResource {
    fn dispose(&self) -> DisposeFuture<'_> {
        Box::pin(async { Ok(()) })
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

#[allow(dead_code)]
impl MemoryManagementResource {
    pub fn management(&self) -> &Arc<dyn crate::runtime::memory::MemoryManagementService> {
        &self.management
    }
}
