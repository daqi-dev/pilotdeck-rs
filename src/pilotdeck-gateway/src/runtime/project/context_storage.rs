//! Project-generation context storage composition. As in
//! `ProjectContextStorageBundle.ts:20-35`, the generation shares storage
//! construction, not context/turn state. Each session composes its own runtime
//! and tool-result budget (`SessionContextRuntimeBundle.ts:83-89,155-187`).

use super::{
    Family, FamilyResource, Resource, ResourceFamilySource, RuntimeError, RuntimeResult,
    StageContext, StageSink,
};
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
        Ok(())
    }
}

/// Immutable native storage factory published in both context storage slots.
/// No runtime, replacement cache, or alias counter belongs to the generation.
pub struct ContextStorageResources {
    snapshot: Arc<JsValue>,
    project_root: PathBuf,
    pilot_home: PathBuf,
    runtime_context_surface: ContextRuntimeContextSurface,
    now: Arc<dyn Fn() -> f64 + Send + Sync>,
}

/// Owned by one session/context composition, including scoped agent contexts.
pub struct SessionContextStorage {
    pub context_runtime: DefaultContextRuntime,
    pub tool_result_budget: Arc<ToolResultBudget>,
}

impl ContextStorageResources {
    /// Build one session/context composition over `tool_results_dir`.
    ///
    /// The caller supplies the session storage's directory (normally
    /// `.pilotdeck/tool-results/<safeId>`) or execution-world spill root, as in
    /// `ProjectSessionRuntimeBundle.ts:520,574`. There is no project-flat
    /// default.
    ///
    /// **Isolation.** Every call owns an independent in-memory replacement
    /// cache and [`ToolResultBudget`]. The directory itself is not namespaced
    /// per session: [`ToolResultBudget`] derives the spill file name from the
    /// *sanitized* scoped `(turnId, toolCallId)` key (a run of characters other
    /// than `[A-Za-z0-9_.-]` becomes one `-`, edge dashes are trimmed, and an
    /// empty `toolCallId` becomes `tool-call`) and derives the read alias from
    /// its own counter. Callers sharing one root must therefore keep that
    /// derived file name unique within the root; uniqueness of the raw scoped
    /// key is not sufficient, and callers must not rely on unsafe characters
    /// to keep results apart. Distinct raw keys can sanitize to the same name,
    /// for example `c/1` with `c-1`, `c--` with `c`, or `""` with
    /// `"tool-call"`. Within one budget the replacement cache is keyed by the
    /// same sanitized key, so a later sanitize-equal tool result returns the
    /// earlier record without writing. If two contexts in one directory
    /// collide, the second write leaves the first file in place (so the second
    /// result is not persisted) while its preview describes the second result:
    /// the second context's record references the first context's artifact.
    /// The measured TS baseline exhibits the same behavior for this input;
    /// this API makes no shared-directory filename-isolation promise.
    /// `context_storage_shared_directory_requires_unique_scoped_keys` locks
    /// the identical-raw-key input boundary,
    /// `context_storage_shared_directory_requires_unique_sanitized_filenames`
    /// locks the sanitize-equal boundary (`c/1` versus `c-1`), and
    /// `context_storage_shared_directory_distinguishes_unique_scoped_keys`
    /// locks the unique-key positive control. This API isolates session state,
    /// not a shared directory's filename namespace.
    ///
    /// **Executor isolation.** [`ToolResultBudget`] offloads the spill write,
    /// alias copy and their path checks to `spawn_blocking`
    /// (`write_text_if_absent`/`copy_file_if_absent`), so `apply_tool_results`
    /// never holds a tokio worker through the blocking fs calls. This matches
    /// the TS spill port, whose `node:fs/promises` writes run off the event
    /// loop, and the storage rule in `runtime/context/instructions.rs` (#637).
    /// Per-session setup keeps one synchronous `current_dir` read on the
    /// caller thread (a single getcwd, no spill I/O; `ToolResultBudget::new`).
    /// Cancellation: one text result's whole persistence sequence — spill
    /// write, alias copy and replacement-cache registration — is a single
    /// blocking task behind one await, and a spawned blocking task is never
    /// aborted, so dropping the caller's future leaves the same state TS
    /// leaves when an abandoned async function still runs every step
    /// unobserved; only the caller stops observing the result. Media spills
    /// complete their write the same way (without an alias). A caller dropped
    /// between two tool-result blocks simply stops the loop there: those
    /// blocks are not spilled yet, and a retry re-runs them.
    pub fn create_session(
        &self,
        tool_results_dir: impl Into<PathBuf>,
    ) -> Result<SessionContextStorage, RuntimeError> {
        let max_context_tokens = self.max_context_tokens()?;
        let tool_result_budget = Arc::new(ToolResultBudget::new(ToolResultBudgetOptions::new(
            tool_results_dir,
        )));
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
                self.project_root.clone(),
                self.project_root.clone(),
                self.pilot_home.clone(),
            )),
            tool_result_budget: Some(tool_result_budget.clone()),
            project_root: Some(self.project_root.to_string_lossy().into_owned()),
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
