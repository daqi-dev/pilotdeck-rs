//! Memory bundle family: provider composition, maintenance and management
//! surfaces (TS ProjectMemoryBundle.ts:41-86).
//!
//! The bundle is registered as a dispose owner before its `stage()` can throw,
//! matching the baseline's field assignment order. Disposal awaits the
//! provider's own cleanup.
//!
//! Provider construction follows `ProjectMemoryBundle.ts:57-73`: the
//! application-selected `providerFactory` wins; otherwise the deprecated
//! `createProvider` is used; otherwise the native EdgeClaw factory is the
//! baseline default. That native EdgeClaw factory is not ported here, so an
//! enabled `edgeclaw` configuration fails explicitly instead of succeeding or
//! being silently disabled.

use super::{
    DisposeFuture, Family, FamilyResource, Resource, ResourceFamilySource, RuntimeError,
    RuntimeResult, StageContext, StageSink,
};
use crate::runtime::context::memory::MemoryResolver;
use crate::runtime::memory::{
    MemoryConfig, MemoryMaintenanceService, MemoryManagementService, MemoryProviderFactoryFn,
    NativeProviderFactoryFn, ProjectMemoryBundle, ProjectMemoryBundleOptions,
};
use pilotdeck_protocol::JsValue;
use std::{any::Any, sync::Arc};

/// The MemoryBundle family source (ProjectMemoryBundle).
///
/// `MemoryBundleFamilySource::default()` is the native default: no injected
/// provider factory. An application-selected provider is supplied with
/// [`Self::with_provider_factory`] (or the deprecated
/// [`Self::with_create_provider`]).
pub struct MemoryBundleFamilySource {
    provider_factory: Option<MemoryProviderFactoryFn>,
    create_provider: Option<NativeProviderFactoryFn>,
}

impl Default for MemoryBundleFamilySource {
    fn default() -> Self {
        Self::new()
    }
}

impl MemoryBundleFamilySource {
    /// No injected provider. The native EdgeClaw default is not ported, so an
    /// enabled `edgeclaw` config returns `NativeProviderNotPorted`; a missing
    /// or disabled config stages no provider.
    pub fn new() -> Self {
        Self {
            provider_factory: None,
            create_provider: None,
        }
    }

    /// Inject the application-selected `providerFactory`. As in
    /// `ProjectMemoryBundle.ts:57`, it takes precedence over
    /// [`Self::with_create_provider`] when both are present.
    pub fn with_provider_factory(mut self, factory: MemoryProviderFactoryFn) -> Self {
        self.provider_factory = Some(factory);
        self
    }

    /// Inject the deprecated `createProvider` hook. It is consulted only when
    /// no `providerFactory` was injected.
    pub fn with_create_provider(mut self, factory: NativeProviderFactoryFn) -> Self {
        self.create_provider = Some(factory);
        self
    }
}

impl ResourceFamilySource for MemoryBundleFamilySource {
    fn family(&self) -> Family {
        Family::MemoryBundle
    }

    fn stage(&self, context: &StageContext, sink: &mut StageSink<'_>) -> RuntimeResult {
        let snapshot = context.snapshot();
        let options = ProjectMemoryBundleOptions {
            // `ProjectRuntimeResourcesBundle.ts:387-398`: the real project
            // root, normalized `snapshot.config.memory`, and
            // `snapshot.config.agent.model.id`.
            project_root: context.project_root(),
            config: memory_config(&snapshot),
            agent_model: agent_model(&snapshot),
            provider_factory: self.provider_factory.clone(),
            create_provider: self.create_provider.clone(),
        };

        let bundle = Arc::new(ProjectMemoryBundle::new(options));

        // `this.memoryBundle = memoryBundle` precedes `memoryBundle.stage()`
        // (ProjectRuntimeResourcesBundle.ts:397-398): the dispose owner is
        // registered even when staging throws.
        sink.register_owner(
            Family::MemoryBundle,
            Arc::new(MemoryBundleResource {
                bundle: Arc::clone(&bundle),
            }),
        );

        // The existing `From<MemoryBundleError> for RuntimeError` keeps the
        // original message unchanged; do not add a staging prefix here.
        let resources = bundle.stage().map_err(RuntimeError::from)?;

        // `Object.assign(this.resources, memoryBundle.stage())`: an absent
        // provider publishes no memory resource at all.
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

/// Normalized `snapshot.config.memory` as the selection fields the memory
/// bundle consumes. The snapshot normalization supplies `enabled` (default
/// true) and `provider` (default `edgeclaw`); `None` means the config has no
/// memory section at all.
fn memory_config(snapshot: &JsValue) -> Option<MemoryConfig> {
    let memory = snapshot.get("config")?.get("memory")?;
    Some(MemoryConfig {
        enabled: memory.get("enabled")?.as_bool()?,
        provider: memory.get("provider")?.as_string()?.to_utf8().ok()?,
    })
}

/// `snapshot.config.agent.model.id` (normalized by `config::snapshot::agent`).
fn agent_model(snapshot: &JsValue) -> Option<String> {
    snapshot
        .get("config")?
        .get("agent")?
        .get("model")?
        .get("id")?
        .as_string()?
        .to_utf8()
        .ok()
}

/// FamilyResource wrapper for the ProjectMemoryBundle (dispose owner).
pub struct MemoryBundleResource {
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
pub struct MemoryResolverResource {
    resolver: Arc<dyn MemoryResolver>,
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

impl MemoryResolverResource {
    pub fn resolver(&self) -> &Arc<dyn MemoryResolver> {
        &self.resolver
    }
}

/// FamilyResource wrapper for memory maintenance service.
pub struct MemoryMaintenanceResource {
    maintenance: Arc<dyn MemoryMaintenanceService>,
}

impl FamilyResource for MemoryMaintenanceResource {
    fn dispose(&self) -> DisposeFuture<'_> {
        Box::pin(async { Ok(()) })
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

impl MemoryMaintenanceResource {
    pub fn maintenance(&self) -> &Arc<dyn MemoryMaintenanceService> {
        &self.maintenance
    }
}

/// FamilyResource wrapper for memory management service.
pub struct MemoryManagementResource {
    management: Arc<dyn MemoryManagementService>,
}

impl FamilyResource for MemoryManagementResource {
    fn dispose(&self) -> DisposeFuture<'_> {
        Box::pin(async { Ok(()) })
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

impl MemoryManagementResource {
    pub fn management(&self) -> &Arc<dyn MemoryManagementService> {
        &self.management
    }
}
