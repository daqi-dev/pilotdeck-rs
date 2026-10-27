//! Project-generation context storage composition. As in
//! `ProjectContextStorageBundle.ts:20-35`, the generation shares storage
//! construction, not context/turn state. Each session composes its own runtime
//! and tool-result budget (`SessionContextRuntimeBundle.ts:83-89,155-187`).

use super::{
    Family, FamilyResource, Resource, ResourceFamilySource, RuntimeError, RuntimeResult,
    StageContext, StageSink,
};
use super::{factory::DeferredResource, memory_bundle::MemoryResolverResource};
use crate::runtime::context::tool_budget::{ToolResultBudget, ToolResultBudgetOptions};
use crate::runtime::context::{
    DefaultContextRuntime, InstructionDiscovery, Options as ContextOptions,
    RuntimeContextSurface as ContextRuntimeContextSurface,
};
use pilotdeck_protocol::JsValue;
use std::{any::Any, path::PathBuf, sync::Arc, time::UNIX_EPOCH};

pub struct ContextStorageFamilySource;

impl ResourceFamilySource for ContextStorageFamilySource {
    fn family(&self) -> Family {
        Family::ContextStorage
    }

    fn stage(&self, context: &StageContext, sink: &mut StageSink<'_>) -> RuntimeResult {
        let clock = context.clock();
        let resources = Arc::new(ContextStorageResources {
            snapshot: context.snapshot(),
            project_root: context.project_root(),
            pilot_home: context.pilot_home(),
            // Memory is staged after context storage. Resolve it only when
            // composing a session from the published project generation.
            memory: context.defer_resource(Resource::Memory),
            runtime_context_surface: convert_runtime_context_surface(
                context.profile().runtime_context_surface,
            ),
            now: Arc::new(move || {
                clock()
                    .duration_since(UNIX_EPOCH)
                    .map(|duration| duration.as_secs_f64() * 1000.0)
                    .unwrap_or(0.0)
            }),
        });
        sink.register_resource(Resource::InstructionStorage, resources.clone());
        sink.register_resource(Resource::ToolResultSpill, resources);
        super::persistence::stage(context, sink)?;
        Ok(())
    }
}

/// Immutable native storage factory published in both context storage slots.
/// No runtime, replacement cache, or alias counter belongs to the generation.
pub struct ContextStorageResources {
    snapshot: Arc<JsValue>,
    project_root: PathBuf,
    pilot_home: PathBuf,
    memory: DeferredResource,
    runtime_context_surface: ContextRuntimeContextSurface,
    now: Arc<dyn Fn() -> f64 + Send + Sync>,
}

/// Options for composing a main or scoped context. The caller still owns
/// subagent definition routing; this factory does not implement `createContext`.
#[derive(Default)]
pub struct SessionContextOptions {
    /// Set for a scoped agent whose definition has `memory: "disabled"`.
    /// The default inherits this generation's resolver.
    pub memory_disabled: bool,
    /// `sessionExecution.handle.hostWorkspaceRoot`, when provided. Both memory
    /// retrieval and instruction discovery use it; otherwise the generation's
    /// project root is used (`ProjectSessionRuntimeBundle.ts:518,572`).
    pub host_workspace_root: Option<PathBuf>,
}

/// Owned by one session/context composition. Scoped callers use
/// [`ContextStorageResources::create_session_with_options`] for memory opt-out
/// and execution-world roots; automatic subagent context routing is not wired.
pub struct SessionContextStorage {
    pub context_runtime: DefaultContextRuntime,
    pub tool_result_budget: Arc<ToolResultBudget>,
}

impl ContextStorageResources {
    /// Build one session/context composition over `tool_results_dir`.
    /// Keep the corresponding `RuntimeLease` alive while creating and using
    /// the session: the resolver is owned by that generation's memory bundle.
    /// A retained storage handle alone does not prevent provider disposal.
    /// This default entry inherits memory and uses the generation's project
    /// root. Scoped/execution-world callers select overrides through
    /// [`Self::create_session_with_options`].
    ///
    /// The caller supplies the session storage's directory (normally
    /// `.pilotdeck/tool-results/<safeId>`) or execution-world spill root, as in
    /// `ProjectSessionRuntimeBundle.ts:520,574`. There is no project-flat
    /// default.
    ///
    /// **Isolation.** Each budget prefixes flat filenames with its own UUID under
    /// the supplied root; identical or sanitize-equal IDs in different contexts cannot
    /// alias a spill file. Read aliases remain exclusive-created under refs.
    /// Within one budget a sanitized key still identifies its cached record.
    /// Expired writes are retired after the bounded wait; their successors
    /// use different physical files, so an uncancellable blocking writer
    /// cannot corrupt or publish over its successor. See the unapproved
    /// difference A7.4-TOOLBUDGET-CONTEXT-ISOLATION-001.
    ///
    /// **Executor isolation.** [`ToolResultBudget`] offloads the spill write,
    /// alias copy and their path checks to `spawn_blocking`
    /// (`write_text_if_absent`/`copy_file_if_absent`), so `apply_tool_results`
    /// never holds a tokio worker through the blocking fs calls. This matches
    /// the TS spill port, whose `node:fs/promises` writes run off the event
    /// loop, and the storage rule in `runtime/context/instructions.rs` (#637).
    /// Per-session setup keeps one synchronous `current_dir` read on the
    /// caller thread (a single getcwd, no spill I/O; `ToolResultBudget::new`,
    /// which #885 turns into an error for a deleted cwd and a relative dir).
    /// Cancellation: one text result's whole persistence sequence — spill
    /// write, alias copy and replacement-cache registration — is a single
    /// blocking task behind one await, and a spawned blocking task is never
    /// aborted, so dropping the caller's future leaves the same state TS
    /// leaves when an abandoned async function still runs every step
    /// unobserved while its lease remains valid. Expiry prevents cache
    /// publication and releases the slot on the next claim or timed wait. Media spills
    /// complete their write the same way (without an alias). A caller dropped
    /// between two tool-result blocks simply stops the loop there: those
    /// blocks are not spilled yet, and a retry re-runs them.
    pub fn create_session(
        &self,
        tool_results_dir: impl Into<PathBuf>,
    ) -> Result<SessionContextStorage, RuntimeError> {
        self.create_session_with_options(tool_results_dir, SessionContextOptions::default())
    }

    /// Compose with scoped memory and execution-world options. The lease and
    /// spill-directory contracts of [`Self::create_session`] apply here too.
    /// For a scoped agent, translate `definition.memory === "disabled"` to
    /// `memory_disabled`; the caller decides whether a separate context is
    /// needed (`ProjectSessionRuntimeBundle.ts:568-595`).
    pub fn create_session_with_options(
        &self,
        tool_results_dir: impl Into<PathBuf>,
        options: SessionContextOptions,
    ) -> Result<SessionContextStorage, RuntimeError> {
        let max_context_tokens = self.max_context_tokens()?;
        // Even an opted-out context must respect the generation's lifetime.
        let memory_resolver = self
            .memory
            .resolve()?
            .filter(|_| !options.memory_disabled)
            .map(|resource| {
                resource
                    .as_any()
                    .downcast_ref::<MemoryResolverResource>()
                    .map(|memory| memory.resolver().clone())
                    .ok_or_else(|| {
                        RuntimeError::new("Project memory resource is not a memory resolver.")
                    })
            })
            .transpose()?;
        let project_root = options
            .host_workspace_root
            .unwrap_or_else(|| self.project_root.clone());
        // #885: the constructor's `resolve` may reject a relative spill dir
        // against a deleted cwd; TS's throw surfaces as a session-creation
        // failure. This composition boundary keeps the message verbatim, so the
        // host chain reports it unchanged instead of as a host execution
        // failure (see `TurnHostFactory::assemble`).
        let tool_result_budget = Arc::new(
            ToolResultBudget::new(ToolResultBudgetOptions::new(tool_results_dir))
                .map_err(|error| RuntimeError::new(error.to_string()))?,
        );
        let memory_retrieval_timeout_ms = self
            .snapshot
            .get("config")
            .and_then(|config| config.get("memory"))
            .and_then(|memory| memory.get("retrievalTimeoutMs"))
            .and_then(JsValue::as_number)
            .unwrap_or(30_000.0);
        let context_runtime = DefaultContextRuntime::new(ContextOptions {
            max_context_tokens,
            runtime_context_surface: self.runtime_context_surface,
            instruction_discovery: Some(InstructionDiscovery::from_environment(
                project_root.clone(),
                project_root.clone(),
                self.pilot_home.clone(),
            )),
            tool_result_budget: Some(tool_result_budget.clone()),
            project_root: Some(project_root.to_string_lossy().into_owned()),
            memory_resolver,
            memory_retrieval_timeout_ms,
            now: Some(self.now.clone()),
            ..ContextOptions::default()
        });
        Ok(SessionContextStorage {
            context_runtime,
            tool_result_budget,
        })
    }

    fn max_context_tokens(&self) -> Result<f64, RuntimeError> {
        let config = self
            .snapshot
            .get("config")
            .ok_or_else(|| RuntimeError::new("Context storage config is missing."))?;
        let agent = config
            .get("agent")
            .ok_or_else(|| RuntimeError::new("Context storage agent config is missing."))?;
        let route = agent.get("model");
        let provider = route
            .and_then(|route| route.get("provider"))
            .and_then(JsValue::as_string)
            .ok_or_else(|| RuntimeError::new("Agent model provider is missing."))?;
        let model = route
            .and_then(|route| route.get("model"))
            .and_then(JsValue::as_string)
            .ok_or_else(|| RuntimeError::new("Agent model name is missing."))?;
        // ModelRuntime.ts:27-47 reads this generation's normalized catalog.
        // Resolve the model even with an override, matching the unconditional
        // getCapabilities before `agent.maxContextTokens ?? caps.maxContextTokens`
        // in ProjectSessionRuntimeBundle.ts:513-533.
        let provider_config = config
            .get("model")
            .and_then(|config| config.get("providers"))
            .and_then(JsValue::as_object)
            .and_then(|providers| providers.get_key(provider))
            .ok_or_else(|| {
                RuntimeError::new(format!(
                    "Provider {} does not exist.",
                    String::from_utf16_lossy(provider.units())
                ))
            })?;
        let model_config = provider_config
            .get("models")
            .and_then(JsValue::as_object)
            .and_then(|models| models.get_key(model))
            .ok_or_else(|| {
                RuntimeError::new(format!(
                    "Model {} does not exist in provider {}.",
                    String::from_utf16_lossy(model.units()),
                    String::from_utf16_lossy(provider.units())
                ))
            })?;
        agent
            .get("maxContextTokens")
            .filter(|value| !matches!(value, JsValue::Null | JsValue::Undefined))
            .or_else(|| {
                model_config
                    .get("capabilities")
                    .and_then(|caps| caps.get("maxContextTokens"))
            })
            .and_then(JsValue::as_number)
            .ok_or_else(|| RuntimeError::new("Model maxContextTokens must be a number."))
    }
}

impl FamilyResource for ContextStorageResources {
    fn dispose(&self) -> super::DisposeFuture<'_> {
        // The baseline storage bundle has no dispose; sessions own their state.
        Box::pin(async { Ok(()) })
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

fn convert_runtime_context_surface(
    value: super::RuntimeContextSurface,
) -> ContextRuntimeContextSurface {
    match value {
        super::RuntimeContextSurface::SystemPrompt => ContextRuntimeContextSurface::SystemPrompt,
        super::RuntimeContextSurface::UserMessage => ContextRuntimeContextSurface::UserMessage,
    }
}
