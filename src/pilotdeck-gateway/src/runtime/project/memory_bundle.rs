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
//!
//! #636 closes the type boundary: [`ProjectMemoryBundleOptions`] now carries
//! the normalized `modelConfig`, the project clock and the opaque telemetry
//! client in addition to the full normalized memory config, so an injected
//! provider factory receives the same composition inputs as
//! `ProjectRuntimeResourcesBundle.ts:387-398`. `From<MemoryBundleError> for
//! RuntimeError` retains the factory's original JS `{name, code, message,
//! requires_host}` instead of collapsing it to `message`.
//!
//! The native EdgeClaw provider itself is still not ported (that is #587), so
//! an enabled `edgeclaw` configuration without an injected factory continues to
//! fail explicitly.

use super::{
    DisposeFuture, Family, FamilyResource, Resource, ResourceFamilySource, RuntimeError,
    RuntimeResult, StageContext, StageSink,
};
use crate::runtime::context::memory::MemoryResolver;
use crate::runtime::memory::{
    MemoryCaptureStrategy, MemoryConfig, MemoryMaintenanceService, MemoryManagementService,
    MemoryModelConfig, MemoryModelProvider, MemoryProviderFactoryFn, MemoryScheduleConfig,
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
            // root, normalized `snapshot.config.memory` / `snapshot.config.model`,
            // `snapshot.config.agent.model.id`, `options.now` and
            // `options.telemetry`.
            project_root: context.project_root(),
            config: memory_config(&snapshot),
            model_config: model_config(&snapshot),
            agent_model: agent_model(&snapshot),
            now: Some(context.clock()),
            telemetry: context.telemetry(),
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

        // `From<MemoryBundleError> for RuntimeError` keeps the original JS
        // error shape and message; do not add a staging prefix here.
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

/// Normalized `snapshot.config.memory` (`PilotMemoryConfig`) as the fields the
/// bundle carries. An absent `memory` section stays absent (`None`) and stages
/// no provider, matching `parseMemoryConfig.ts` returning `undefined` (and the
/// native factory then returning `undefined`) for a missing section. When the
/// section is present, normalization supplies the required fields; the
/// optional fields map to `None` without inventing a default.
fn memory_config(snapshot: &JsValue) -> Option<MemoryConfig> {
    let memory = snapshot.get("config")?.get("memory")?;
    let schedule = memory
        .get("schedule")
        .and_then(JsValue::as_object)
        .map(|schedule| MemoryScheduleConfig {
            reasoning_mode: text(schedule.get("reasoningMode")),
            auto_index_interval_minutes: schedule
                .get("autoIndexIntervalMinutes")
                .and_then(JsValue::as_number),
            auto_dream_interval_minutes: schedule
                .get("autoDreamIntervalMinutes")
                .and_then(JsValue::as_number),
        });
    Some(MemoryConfig {
        enabled: memory.get("enabled")?.as_bool()?,
        provider: memory.get("provider")?.as_string()?.to_utf8().ok()?,
        root_dir: text(memory.get("rootDir")).unwrap_or_default(),
        capture_strategy: match text(memory.get("captureStrategy")).as_deref() {
            Some("full_session") => MemoryCaptureStrategy::FullSession,
            _ => MemoryCaptureStrategy::LastTurn,
        },
        include_assistant: memory
            .get("includeAssistant")
            .and_then(JsValue::as_bool)
            .unwrap_or(true),
        max_message_chars: memory.get("maxMessageChars").and_then(JsValue::as_number),
        retrieval_timeout_ms: memory
            .get("retrievalTimeoutMs")
            .and_then(JsValue::as_number),
        model: text(memory.get("model")),
        api_type: text(memory.get("apiType")),
        schedule,
        heartbeat_batch_size: memory
            .get("heartbeatBatchSize")
            .and_then(JsValue::as_number),
    })
}

/// `snapshot.config.model` reduced to what `createEdgeClawMemoryProviderFromConfig`
/// reads from `ModelConfig.providers[provider]`
/// (`createEdgeClawMemoryProviderFromConfig.ts:91-98`). The api key is carried
/// for the provider factory only and is never logged.
fn model_config(snapshot: &JsValue) -> Option<MemoryModelConfig> {
    let providers = snapshot
        .get("config")?
        .get("model")?
        .get("providers")?
        .as_object()?;
    let mut config = MemoryModelConfig::default();
    for (id, provider) in providers.entries() {
        let Ok(id) = id.to_utf8() else {
            continue;
        };
        let Some(provider) = provider.as_object() else {
            continue;
        };
        config.providers.insert(
            id,
            MemoryModelProvider {
                protocol: text(provider.get("protocol")),
                url: text(provider.get("url")),
                api_key: text(provider.get("apiKey")),
            },
        );
    }
    Some(config)
}

fn text(value: Option<&JsValue>) -> Option<String> {
    value?.as_string()?.to_utf8().ok()
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
