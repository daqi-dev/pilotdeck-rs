//! Context storage family: instruction discovery, tool-result budgeting, and
//! the DefaultContextRuntime (TS ProjectContextStorageBundle.ts:14-42).
//!
//! This family has no rollback handle (the baseline never disposes it), so it
//! only registers published resources through the sink.

use super::{
    Family, FamilyResource, Resource, ResourceFamilySource, RuntimeResult, StageContext, StageSink,
};
use crate::runtime::context::tool_budget::{ToolResultBudget, ToolResultBudgetOptions};
use crate::runtime::context::{
    DefaultContextRuntime, InstructionDiscovery, Options as ContextOptions,
    RuntimeContextSurface as ContextRuntimeContextSurface,
};
use std::any::Any;
use std::sync::Arc;
use std::time::UNIX_EPOCH;

/// The ContextStorage family source (ProjectContextStorageBundle).
pub struct ContextStorageFamilySource;

impl ResourceFamilySource for ContextStorageFamilySource {
    fn family(&self) -> Family {
        Family::ContextStorage
    }

    fn stage(&self, context: &StageContext, sink: &mut StageSink<'_>) -> RuntimeResult {
        let snapshot = context.snapshot();
        let profile = context.profile();
        let project_root = context.project_root();
        let pilot_home = context.pilot_home();
        let _env = context.env();

        // Instruction discovery from config
        // TS baseline enables it by default unless explicitly set to false
        let instruction_discovery = {
            let enabled = snapshot
                .get("config")
                .and_then(|config| config.get("agent"))
                .and_then(|agent| agent.get("instructionDiscovery"))
                .and_then(|value| value.as_bool())
                .unwrap_or(true); // default is enabled

            if enabled {
                // Use from_environment to properly handle PILOTDECK_MANAGED_CONFIG
                // with empty-string filtering and deferred environment reading
                Some(InstructionDiscovery::from_environment(
                    project_root.clone(),
                    project_root.clone(),
                    pilot_home.clone(),
                ))
            } else {
                None
            }
        };

        // Tool result budget: requires tool_results_dir
        let tool_results_dir = project_root.join(".pilotdeck").join("tool-results");
        let tool_result_budget = Some(Arc::new(ToolResultBudget::new(
            ToolResultBudgetOptions::new(tool_results_dir),
        )));

        // Build a clock adapter that converts SystemTime to f64 (ms since epoch)
        let clock = context.clock();
        let now_fn: Arc<dyn Fn() -> f64 + Send + Sync> = Arc::new(move || {
            let time = clock();
            time.duration_since(UNIX_EPOCH)
                .map(|d| d.as_secs_f64() * 1000.0)
                .unwrap_or(0.0)
        });

        // Build DefaultContextRuntime with available resources
        let context_runtime = DefaultContextRuntime::new(ContextOptions {
            compaction: None, // Compaction provider is separate family
            max_context_tokens: 8192.0,
            truncate_first_keep_ratio: 0.5,
            extension: None,
            include_extensions_with_custom_system_prompt: false,
            runtime_context_surface: convert_runtime_context_surface(
                profile.runtime_context_surface,
            ),
            instruction_discovery,
            prompt_contributions: None,
            tool_result_budget: tool_result_budget.clone(),
            memory_resolver: None, // Memory is separate family
            project_root: Some(project_root.to_string_lossy().into_owned()),
            memory_retrieval_timeout_ms: 30_000.0,
            prompt_cache_coordinator: None, // Separate family
            now: Some(now_fn),
            platform: None,
            node_version: None,
            locale: None,
        });

        // Register InstructionStorage resource (the DefaultContextRuntime's instruction layer)
        sink.register_resource(
            Resource::InstructionStorage,
            Arc::new(InstructionStorageResource {
                context_runtime: Arc::new(context_runtime),
            }),
        );

        // Register ToolResultSpill (the budget handle)
        if let Some(budget) = tool_result_budget {
            sink.register_resource(
                Resource::ToolResultSpill,
                Arc::new(ToolResultSpillResource { budget }),
            );
        }

        Ok(())
    }
}

/// FamilyResource wrapper for the instruction storage (DefaultContextRuntime).
struct InstructionStorageResource {
    context_runtime: Arc<DefaultContextRuntime>,
}

impl FamilyResource for InstructionStorageResource {
    fn dispose(&self) -> super::DisposeFuture<'_> {
        // Context storage is never disposed (ProjectContextStorageBundle has no dispose)
        Box::pin(async { Ok(()) })
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

#[allow(dead_code)]
impl InstructionStorageResource {
    pub fn context_runtime(&self) -> &Arc<DefaultContextRuntime> {
        &self.context_runtime
    }
}

/// FamilyResource wrapper for tool result spill (ToolResultBudget).
struct ToolResultSpillResource {
    budget: Arc<ToolResultBudget>,
}

impl FamilyResource for ToolResultSpillResource {
    fn dispose(&self) -> super::DisposeFuture<'_> {
        Box::pin(async { Ok(()) })
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

#[allow(dead_code)]
impl ToolResultSpillResource {
    pub fn budget(&self) -> &Arc<ToolResultBudget> {
        &self.budget
    }
}

// Convert profile's RuntimeContextSurface to context module's enum
fn convert_runtime_context_surface(
    value: super::RuntimeContextSurface,
) -> ContextRuntimeContextSurface {
    match value {
        super::RuntimeContextSurface::SystemPrompt => ContextRuntimeContextSurface::SystemPrompt,
        super::RuntimeContextSurface::UserMessage => ContextRuntimeContextSurface::UserMessage,
    }
}
