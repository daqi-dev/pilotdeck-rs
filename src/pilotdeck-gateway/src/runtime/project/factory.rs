//! Production [`ProjectRuntimeFactory`]: the staging sequence of the fixed
//! TypeScript baseline's `ProjectRuntimeRegistry.buildRuntime`
//! (ProjectRuntimeRegistry.ts:1596-1693), which is the config read plus
//! `ProjectRuntimeResourcesBundle.stage` (ProjectRuntimeResourcesBundle.ts:196-400).
//!
//! The factory owns no lifecycle of its own: the registry (project.rs) stages
//! it once per generation, publishes only fully staged bundles, and drains
//! retired generations before disposal. Each `stage` call reads the current
//! config, so a config reload enters the new generation while leased turns on
//! the old one keep the resources they were admitted with.
//!
//! Resource families (model, router, plugins, execution world, MCP, memory,
//! …) are injected as [`ResourceFamilySource`] constructors, in the baseline's
//! stage order. Until A6.9-pr3b..3e wire them, [`Families::unwired`] reports
//! each one with an explicit error instead of staging silently. The rollback
//! (`ProjectRuntimeResourcesBundle.disposeResources`, lines 408-439) is owned
//! by the single [`ProjectResources`] group registered before the first
//! family step can fail; it disposes in the baseline's order: the mcp
//! provider, then the parallel set started together in array order, then the
//! memory bundle. Context storage and the prompt-cache coordinator have no
//! baseline disposal and are never disposed here either.
//!
//! Not in this port (staged for the family packages): `BackgroundSubagentRuntime`
//! and `projectStorage` (buildRuntime's non-bundle members), the
//! `createSessionTools`/`createSessionExecution` closures, the session-execution
//! storage-root validation (lines 276-332), and the registry options that only
//! reach single families (`extraTools`, `subagentIdFactory`, `builtinPlugins`,
//! telemetry, router event bus / session state / provider health, organization
//! model policy).
use super::{
    DisposeFuture, ProjectRuntimeFactory, ResourceBundle, RuntimeError, RuntimeResources,
    RuntimeResult,
};
use crate::{
    config::{self, Env},
    runtime::plugins::PluginRuntimeOptions,
};
use pilotdeck_protocol::JsValue;
use std::{
    any::Any,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{SystemTime, UNIX_EPOCH},
};

/// `() => Date`; handed to every staged family (the bundle's `options.now`),
/// never to the config read.
pub type Now = Arc<dyn Fn() -> SystemTime + Send + Sync>;

/// `SANDBOX_MODES` / `resolveSandboxMode` (SandboxPort.ts:5-23): an unknown
/// value keeps the native `danger-full-access` fallback.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SandboxMode {
    ReadOnly,
    WorkspaceWrite,
    DangerFullAccess,
}
impl SandboxMode {
    /// `DEFAULT_SANDBOX_MODE` applies to every non-member input.
    fn resolve(value: Option<&str>) -> Self {
        match value {
            Some("read-only") => Self::ReadOnly,
            Some("workspace-write") => Self::WorkspaceWrite,
            _ => Self::DangerFullAccess,
        }
    }
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ReadOnly => "read-only",
            Self::WorkspaceWrite => "workspace-write",
            Self::DangerFullAccess => "danger-full-access",
        }
    }
}

/// `RUNTIME_CONTEXT_SURFACES` / `resolveRuntimeContextSurface`
/// (RuntimeContextSurface.ts:5-24). This is the profile vocabulary, not the
/// branch-preserving enum of `runtime::context`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RuntimeContextSurface {
    SystemPrompt,
    UserMessage,
}
impl RuntimeContextSurface {
    /// `DEFAULT_RUNTIME_CONTEXT_SURFACE` applies to every non-member input.
    fn resolve(value: Option<&str>) -> Self {
        match value {
            Some("user_message") => Self::UserMessage,
            _ => Self::SystemPrompt,
        }
    }
    pub fn as_str(self) -> &'static str {
        match self {
            Self::SystemPrompt => "system_prompt",
            Self::UserMessage => "user_message",
        }
    }
}

/// `INTERACTION_PROFILE_NAMES` (InteractionProfile.ts:6-8).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InteractionProfileName {
    Interactive,
    Headless,
    Disabled,
}
impl InteractionProfileName {
    /// `DEFAULT_INTERACTION_PROFILE_NAME` applies to every non-member input.
    fn resolve(value: Option<&str>) -> Self {
        match value {
            Some("headless") => Self::Headless,
            Some("disabled") => Self::Disabled,
            _ => Self::Interactive,
        }
    }
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Interactive => "interactive",
            Self::Headless => "headless",
            Self::Disabled => "disabled",
        }
    }
}

/// `InteractionProfile` (InteractionProfile.ts:17-51): a frozen selection.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InteractionProfile {
    pub name: InteractionProfileName,
    pub policy_mode: &'static str,
    pub can_prompt: bool,
    pub question_provider: &'static str,
    pub permission_provider: &'static str,
}
impl InteractionProfile {
    /// `PROFILES[profile ?? DEFAULT_INTERACTION_PROFILE_NAME]`.
    fn resolve(name: InteractionProfileName) -> Self {
        match name {
            InteractionProfileName::Interactive => Self {
                name,
                policy_mode: "interactive",
                can_prompt: true,
                question_provider: "gateway",
                permission_provider: "gateway",
            },
            InteractionProfileName::Headless => Self {
                name,
                policy_mode: "headless",
                // A deterministic question answerer is available; permission
                // asks still fail closed without a human answerer.
                can_prompt: true,
                question_provider: "deterministic",
                permission_provider: "fail_closed",
            },
            InteractionProfileName::Disabled => Self {
                name,
                policy_mode: "disabled",
                can_prompt: false,
                question_provider: "disabled",
                permission_provider: "fail_closed",
            },
        }
    }
}

/// `PilotDeckRuntimeProfileOverrides` (PilotDeckRuntimeProfile.ts:31-36).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RuntimeProfileOverrides {
    /// Application-level interaction profile takes precedence over the project
    /// config.
    pub interaction_profile_override: Option<InteractionProfileName>,
    /// Compatibility selection for the legacy automatic elicitation mode.
    pub auto_elicitation: Option<bool>,
}

/// `PilotDeckRuntimeProfile` (PilotDeckRuntimeProfile.ts:17-23): immutable
/// provider selections frozen with one project generation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RuntimeProfile {
    pub sandbox_mode: SandboxMode,
    pub runtime_context_surface: RuntimeContextSurface,
    pub interaction: InteractionProfile,
}
impl RuntimeProfile {
    /// `resolvePilotDeckRuntimeProfile` (PilotDeckRuntimeProfile.ts:39-51)
    /// over `snapshot.config.agent`. Pure, like the baseline's call between
    /// the config read and the first resource family.
    pub fn resolve(agent: &JsValue, overrides: &RuntimeProfileOverrides) -> Self {
        let field = |name: &str| {
            agent
                .get(name)
                .and_then(JsValue::as_string)
                .and_then(|value| value.to_utf8().ok())
        };
        // `input.interactionProfileOverride
        //   ?? (input.autoElicitation ? "headless" : input.agent.interactionProfile)`
        let selected = match (
            overrides.interaction_profile_override,
            overrides.auto_elicitation,
        ) {
            (Some(name), _) => name,
            (None, Some(true)) => InteractionProfileName::Headless,
            (None, _) => InteractionProfileName::resolve(field("interactionProfile").as_deref()),
        };
        Self {
            sandbox_mode: SandboxMode::resolve(field("sandboxMode").as_deref()),
            runtime_context_surface: RuntimeContextSurface::resolve(
                field("runtimeContextSurface").as_deref(),
            ),
            interaction: InteractionProfile::resolve(selected),
        }
    }
}

/// One family's staged handle (one `ProjectRuntimeResources` member). Opaque
/// to the factory; its [`FamilyResource::dispose`] is the baseline's
/// per-resource cleanup, and later families downcast it for their
/// constructor inputs.
pub trait FamilyResource: Any + Send + Sync {
    /// This resource's cleanup as `ProjectRuntimeResourcesBundle.dispose`
    /// performs it. Never called for resources the baseline never disposes
    /// (context storage, prompt-cache coordinator).
    fn dispose(&self) -> DisposeFuture<'_>;
    /// Downcast access: later families receive earlier families' handles as
    /// `dyn FamilyResource` and construct from their concrete types.
    fn as_any(&self) -> &dyn Any;
}

/// The baseline resource families, in stage order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Family {
    /// `ProjectModelRuntimeBundle`.
    ModelRuntime,
    /// `PluginRuntime`.
    PluginRuntime,
    /// `routerSessionCustomRouterFactory?.() ?? createNativeRouterSessionCustomRouterPort()`.
    RouterCustomRouters,
    /// `ProjectRouterRuntimeBundle`.
    RouterRuntime,
    /// `lspServiceFactory?.() ?? new LspService()`.
    LspService,
    /// `ProjectExecutionWorldBundle`.
    ExecutionWorld,
    /// `ProjectContextStorageBundle`.
    ContextStorage,
    /// `compactionProviderFactory?.()`.
    Compaction,
    /// `promptCacheCoordinatorFactory?.()`.
    PromptCacheCoordinator,
    /// `sessionTitleProviderFactory?.() ?? createNativeSessionTitleProvider()`.
    SessionTitleProvider,
    /// `ProjectMcpRuntimeProvider`.
    McpProvider,
    /// `ProjectMemoryBundle`.
    MemoryBundle,
}
impl Family {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ModelRuntime => "modelRuntime",
            Self::PluginRuntime => "pluginRuntime",
            Self::RouterCustomRouters => "routerSessionCustomRouters",
            Self::RouterRuntime => "routerRuntime",
            Self::LspService => "lspService",
            Self::ExecutionWorld => "executionWorld",
            Self::ContextStorage => "contextStorage",
            Self::Compaction => "compaction",
            Self::PromptCacheCoordinator => "promptCacheCoordinator",
            Self::SessionTitleProvider => "sessionTitleProvider",
            Self::McpProvider => "mcpProvider",
            Self::MemoryBundle => "memoryBundle",
        }
    }
}

/// The published resources of `ProjectRuntimeResources` that later families
/// and session consumers read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Resource {
    Model,
    ModelProviders,
    TokenAccounting,
    Router,
    /// Registered by A6.9-pr3e `PluginFamilySource`.
    PluginRuntime,
    /// Registered by A6.9-pr3e `CustomRouterFamilySource`.
    RouterCustomRouters,
    Tools,
    ExecutionWorld,
    /// Factory capability marker for interaction: this generation's builders
    /// selected interaction, independent of the runtime profile. The profile
    /// stays authoritative — a Disabled or Headless profile is not changed by
    /// the marker's presence. The per-turn permission, hooks, event and steer
    /// ports are bound by A7.2-pr6 (#589); this marker alone must not be read
    /// as those ports being ready and must not enable them.
    Interaction,
    InstructionStorage,
    ToolResultSpill,
    PromptCacheCoordinator,
    Memory,
    MemoryService,
    MemoryManagement,
}
impl Resource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Model => "model",
            Self::ModelProviders => "modelProviders",
            Self::TokenAccounting => "tokenAccounting",
            Self::Router => "router",
            Self::PluginRuntime => "pluginRuntime",
            Self::RouterCustomRouters => "routerSessionCustomRouters",
            Self::Tools => "tools",
            Self::ExecutionWorld => "executionWorld",
            Self::Interaction => "interaction",
            Self::InstructionStorage => "instructionStorage",
            Self::ToolResultSpill => "toolResultSpill",
            Self::PromptCacheCoordinator => "promptCacheCoordinator",
            Self::Memory => "memory",
            Self::MemoryService => "memoryService",
            Self::MemoryManagement => "memoryManagement",
        }
    }
}

/// One baseline resource family's construction and staging. `stage` must
/// register each rollback-owned handle exactly when the baseline assigns the
/// corresponding field: the four bundle families (`modelRuntime`,
/// `routerRuntime`, `executionWorldBundle`, `memoryBundle`) assign their
/// field *before* their own `stage()` can throw, so a source whose staging
/// fails must have registered its bundle already.
pub trait ResourceFamilySource: Send + Sync {
    fn family(&self) -> Family;
    /// Construct and stage this family, registering handles through `sink`
    /// and reading earlier families through `context`. Errors abort the whole
    /// generation; already staged families are rolled back.
    fn stage(&self, context: &StageContext, sink: &mut StageSink<'_>) -> RuntimeResult;
    /// Whether this source layers the interaction capability marker over
    /// another execution-world source. [`Families::with_tools_mcp`] consults
    /// it so replacing the execution world does not silently drop the marker
    /// when the builders are chained in either order. Ordinary family sources
    /// answer `false`.
    ///
    /// Contract: the answer must accurately describe whether *this* source
    /// (including any wrapper it delegates to) publishes
    /// [`Resource::Interaction`] when staged. A transparent decorator that
    /// wraps an interaction-layered source must forward the answer rather
    /// than fall back to the default. [`Families::with_tools_mcp`] always
    /// replaces the execution-world kernel with the native source (including
    /// a custom kernel) and rebuilds the marker layering as a fresh
    /// `InteractionFamilySource` around that native source; any decorator
    /// outside it, transparent or not, is replaced too. Only the marker
    /// selection survives.
    /// Sources that answer `true` without publishing the marker, or `false`
    /// while publishing it, are outside this contract: the builders neither
    /// detect nor repair such a false declaration.
    fn layers_interaction(&self) -> bool {
        false
    }
}

/// `loadPilotConfig` as the bundle calls it: `({ projectRoot, env })`
/// (ProjectRuntimeResourcesBundle.ts:201-204).
pub trait ConfigSource: Send + Sync {
    fn load(&self, project_root: &Path) -> Result<Arc<JsValue>, RuntimeError>;
}

/// Native config read over the real filesystem: the ported `loadPilotConfig`
/// (crate::config). The Pilot home is resolved from the injected
/// environment like `resolvePilotHome`, not from the registry's option.
pub struct NativeConfigSource {
    pilot_home: PathBuf,
    cwd: PathBuf,
    env: Env,
    version: f64,
}
impl NativeConfigSource {
    /// `version` is the baseline's `options.version ?? 1`.
    pub fn new(
        cwd: impl Into<PathBuf>,
        env: Env,
        homedir: Option<PathBuf>,
        version: Option<f64>,
    ) -> Self {
        let cwd = cwd.into();
        Self {
            pilot_home: resolve_pilot_home(&env, &cwd, homedir.as_deref()),
            cwd,
            env,
            version: version.unwrap_or(1.0),
        }
    }
}
impl ConfigSource for NativeConfigSource {
    fn load(&self, project_root: &Path) -> Result<Arc<JsValue>, RuntimeError> {
        let project = project_root.to_str().ok_or_else(|| {
            RuntimeError::new("Project root is not valid UTF-8 for the config loader.")
        })?;
        let snapshot = config::load_snapshot(
            &self.pilot_home,
            &self.cwd,
            &self.env,
            Some(project),
            self.version,
            &loaded_at(&SystemTime::now()),
        )
        .map_err(|error| RuntimeError::new(String::from_utf16_lossy(error.message.units())))?;
        Ok(Arc::new(snapshot))
    }
}

/// `new Date().toISOString()` for the snapshot's `loadedAt`. The baseline's
/// `loadPilotConfig` takes its own wall clock (`new Date()`,
/// loadPilotConfig.ts:65) — the bundle passes it only `projectRoot`/`env`
/// (ProjectRuntimeResourcesBundle.ts:201-204), never `options.now` — so the
/// injected resource clock neither stamps nor advances with this read.
fn loaded_at(now: &SystemTime) -> String {
    let ms = match now.duration_since(UNIX_EPOCH) {
        Ok(elapsed) => elapsed.as_millis().min(i64::MAX as u128) as i64,
        Err(error) => -(error.duration().as_millis().min(i64::MAX as u128) as i64),
    };
    pilotdeck_transport::system_clock::format_epoch_iso(ms)
}

/// `resolvePilotHome` (paths.ts:20-22) with `normalizeHomePath` (paths.ts:98-108):
/// `~` expands against the process home directory, `~/rest` resolves against
/// it, and any other value resolves against the process cwd.
fn resolve_pilot_home(env: &Env, cwd: &Path, homedir: Option<&Path>) -> PathBuf {
    let raw = env
        .get("PILOT_HOME")
        .map(String::as_str)
        .unwrap_or("~/.pilotdeck");
    let home = || {
        homedir
            .map(Path::to_path_buf)
            .unwrap_or_else(|| std::env::home_dir().unwrap_or_else(|| PathBuf::from("/")))
    };
    if raw == "~" {
        return home();
    }
    if let Some(rest) = raw.strip_prefix("~/") {
        return crate::metadata::resolve(&home(), rest);
    }
    crate::metadata::resolve(cwd, raw)
}

/// The per-generation family constructors. `None` follows the baseline's
/// default for that option: the three native-default families fall back to
/// [`UnsupportedFamily`] until their port lands, while compaction and the
/// prompt-cache coordinator are absent by default and stage nothing.
pub struct Families {
    pub model_runtime: Arc<dyn ResourceFamilySource>,
    pub plugins: Arc<dyn ResourceFamilySource>,
    pub router_custom_routers: Option<Arc<dyn ResourceFamilySource>>,
    pub router_runtime: Arc<dyn ResourceFamilySource>,
    pub lsp: Option<Arc<dyn ResourceFamilySource>>,
    pub execution_world: Arc<dyn ResourceFamilySource>,
    pub context_storage: Arc<dyn ResourceFamilySource>,
    pub compaction: Option<Arc<dyn ResourceFamilySource>>,
    pub prompt_cache_coordinator: Option<Arc<dyn ResourceFamilySource>>,
    pub session_title: Option<Arc<dyn ResourceFamilySource>>,
    pub mcp: Arc<dyn ResourceFamilySource>,
    pub memory: Arc<dyn ResourceFamilySource>,
}
impl Families {
    /// Wire the native model/provider/router families; other families retain
    /// their existing constructors (A6.9-pr3c..3e).
    pub fn with_native_models(mut self) -> Self {
        self.model_runtime = Arc::new(super::model::ModelFamilySource::default());
        self.router_runtime = Arc::new(super::model::RouterFamilySource::default());
        self
    }

    /// Wire the MCP and execution-world families (A6.9-pr3c1). Replacing the
    /// execution-world source is the point of this builder: a previously
    /// configured custom source is discarded in favour of the native one and
    /// is never staged. If the interaction marker is already layered over the
    /// execution world, keep the marker layering by wrapping the new native
    /// source: only the marker selection is preserved, not the discarded
    /// custom kernel. The rebuilt layering is a fresh
    /// `InteractionFamilySource` around the native source, so a decorator that
    /// wrapped the custom kernel is replaced as well. That makes
    /// `with_interaction` and `with_tools_mcp` independent of call order.
    pub fn with_tools_mcp(mut self) -> Self {
        self.mcp = Arc::new(super::tools_mcp::McpFamilySource);
        let execution_world: Arc<dyn ResourceFamilySource> =
            Arc::new(super::tools_mcp::ExecutionWorldFamilySource);
        self.execution_world = if self.execution_world.layers_interaction() {
            Arc::new(super::interaction::InteractionFamilySource::new(
                execution_world,
            ))
        } else {
            execution_world
        };
        self
    }

    /// Wire the plugin runtime and custom router registry (A6.9-pr3e).
    /// The plugin runtime is constructed with the given options; the custom
    /// router registry is backed by that same plugin runtime.
    pub fn with_plugins(mut self, options: PluginRuntimeOptions) -> Self {
        self.plugins = Arc::new(super::plugins::PluginFamilySource { options });
        self.router_custom_routers =
            Some(Arc::new(super::custom_routers::CustomRouterFamilySource));
        self
    }

    /// Publish the interaction capability marker (permission, hooks, event,
    /// steer) for A6.9-pr3c2. This is a factory-level marker on the
    /// execution-world family, not the per-turn ports: their binding stays
    /// with A7.2-pr6 (#589). It records only the builder selection and is
    /// published regardless of `context.profile()`: consumers keep obeying
    /// the frozen runtime profile (Interactive/Headless/Disabled) and must
    /// not enable interaction paths from this marker alone.
    ///
    /// Idempotent: calling this on an already-layered source keeps the same
    /// wrapper instead of wrapping it again. The wrapper preserves whatever
    /// execution-world source is currently configured; a later
    /// `with_tools_mcp` replaces that inner source with the native one (see
    /// [`Self::with_tools_mcp`]) and keeps the marker.
    pub fn with_interaction(mut self) -> Self {
        if !self.execution_world.layers_interaction() {
            let execution_world = Arc::clone(&self.execution_world);
            self.execution_world = Arc::new(super::interaction::InteractionFamilySource::new(
                execution_world,
            ));
        }
        self
    }

    /// Every family not yet wired into the native runtime (A6.9-pr3b..3e).
    /// Staging reports each one explicitly instead of succeeding silently.
    pub fn unwired() -> Self {
        Self {
            model_runtime: unsupported(Family::ModelRuntime, "A6.9-pr3b"),
            plugins: unsupported(Family::PluginRuntime, "A6.9-pr3e"),
            router_custom_routers: None,
            router_runtime: unsupported(Family::RouterRuntime, "A6.9-pr3b"),
            lsp: None,
            execution_world: unsupported(Family::ExecutionWorld, "A6.9-pr3c"),
            context_storage: unsupported(Family::ContextStorage, "A6.9-pr3d"),
            compaction: None,
            prompt_cache_coordinator: None,
            session_title: None,
            mcp: unsupported(Family::McpProvider, "A6.9-pr3c"),
            memory: unsupported(Family::MemoryBundle, "A6.9-pr3d"),
        }
    }
}

/// Stands in for a family whose native construction is not ported yet.
pub struct UnsupportedFamily {
    family: Family,
    detail: &'static str,
}
impl UnsupportedFamily {
    pub fn new(family: Family, detail: &'static str) -> Self {
        Self { family, detail }
    }
}
impl ResourceFamilySource for UnsupportedFamily {
    fn family(&self) -> Family {
        self.family
    }
    fn stage(&self, _: &StageContext, _: &mut StageSink<'_>) -> RuntimeResult {
        Err(RuntimeError::new(format!(
            "Project runtime resource family {} is not available in the native runtime yet ({}).",
            self.family.as_str(),
            self.detail
        )))
    }
}

fn unsupported(family: Family, detail: &'static str) -> Arc<dyn ResourceFamilySource> {
    Arc::new(UnsupportedFamily::new(family, detail))
}

/// One generation's staged state: the config inputs plus every handle the
/// families registered. Shared by the [`StageContext`] read view, the
/// [`StageSink`] write view, and the published [`ProjectResources`] group.
struct Slots {
    project_root: PathBuf,
    pilot_home: PathBuf,
    builtin_skills_root: Option<PathBuf>,
    env: Env,
    now: Now,
    snapshot: Arc<JsValue>,
    profile: RuntimeProfile,
    // Dispose owners, keyed by the baseline field that owns cleanup.
    mcp_provider: Option<Arc<dyn FamilyResource>>,
    plugin_runtime: Option<Arc<dyn FamilyResource>>,
    router_custom_routers: Option<Arc<dyn FamilyResource>>,
    compaction: Option<Arc<dyn FamilyResource>>,
    session_title_provider: Option<Arc<dyn FamilyResource>>,
    lsp_service: Option<Arc<dyn FamilyResource>>,
    execution_world_bundle: Option<Arc<dyn FamilyResource>>,
    router_runtime: Option<Arc<dyn FamilyResource>>,
    model_runtime: Option<Arc<dyn FamilyResource>>,
    memory_bundle: Option<Arc<dyn FamilyResource>>,
    // Staged outputs (`ProjectRuntimeResources` members).
    model: Option<Arc<dyn FamilyResource>>,
    model_providers: Option<Arc<dyn FamilyResource>>,
    token_accounting: Option<Arc<dyn FamilyResource>>,
    router: Option<Arc<dyn FamilyResource>>,
    plugin_runtime_resource: Option<Arc<dyn FamilyResource>>,
    router_custom_routers_resource: Option<Arc<dyn FamilyResource>>,
    tools: Option<Arc<dyn FamilyResource>>,
    execution_world: Option<Arc<dyn FamilyResource>>,
    interaction: Option<Arc<dyn FamilyResource>>,
    instruction_storage: Option<Arc<dyn FamilyResource>>,
    tool_result_spill: Option<Arc<dyn FamilyResource>>,
    prompt_cache_coordinator: Option<Arc<dyn FamilyResource>>,
    memory: Option<Arc<dyn FamilyResource>>,
    memory_service: Option<Arc<dyn FamilyResource>>,
    memory_management: Option<Arc<dyn FamilyResource>>,
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Read view of one in-progress (or published) generation.
pub struct StageContext {
    slots: Arc<Mutex<Slots>>,
}
impl StageContext {
    pub fn project_root(&self) -> PathBuf {
        lock(&self.slots).project_root.clone()
    }
    pub fn pilot_home(&self) -> PathBuf {
        lock(&self.slots).pilot_home.clone()
    }
    pub fn builtin_skills_root(&self) -> Option<PathBuf> {
        lock(&self.slots).builtin_skills_root.clone()
    }
    pub fn env(&self) -> Env {
        lock(&self.slots).env.clone()
    }
    pub fn snapshot(&self) -> Arc<JsValue> {
        Arc::clone(&lock(&self.slots).snapshot)
    }
    pub fn profile(&self) -> RuntimeProfile {
        lock(&self.slots).profile
    }
    pub fn clock(&self) -> Now {
        lock(&self.slots).now.clone()
    }
    pub fn now(&self) -> SystemTime {
        (lock(&self.slots).now)()
    }
    /// A family's dispose owner, once its source has registered it.
    pub fn owner(&self, family: Family) -> Option<Arc<dyn FamilyResource>> {
        let slots = lock(&self.slots);
        match family {
            Family::ModelRuntime => slots.model_runtime.clone(),
            Family::PluginRuntime => slots.plugin_runtime.clone(),
            Family::RouterCustomRouters => slots.router_custom_routers.clone(),
            Family::RouterRuntime => slots.router_runtime.clone(),
            Family::LspService => slots.lsp_service.clone(),
            Family::ExecutionWorld => slots.execution_world_bundle.clone(),
            Family::ContextStorage => None,
            Family::Compaction => slots.compaction.clone(),
            Family::PromptCacheCoordinator => None,
            Family::SessionTitleProvider => slots.session_title_provider.clone(),
            Family::McpProvider => slots.mcp_provider.clone(),
            Family::MemoryBundle => slots.memory_bundle.clone(),
        }
    }
    /// A published resource, once a family has staged it.
    pub fn resource(&self, resource: Resource) -> Option<Arc<dyn FamilyResource>> {
        let slots = lock(&self.slots);
        match resource {
            Resource::Model => slots.model.clone(),
            Resource::ModelProviders => slots.model_providers.clone(),
            Resource::TokenAccounting => slots.token_accounting.clone(),
            Resource::Router => slots.router.clone(),
            Resource::PluginRuntime => slots.plugin_runtime_resource.clone(),
            Resource::RouterCustomRouters => slots.router_custom_routers_resource.clone(),
            Resource::Tools => slots.tools.clone(),
            Resource::ExecutionWorld => slots.execution_world.clone(),
            Resource::Interaction => slots.interaction.clone(),
            Resource::InstructionStorage => slots.instruction_storage.clone(),
            Resource::ToolResultSpill => slots.tool_result_spill.clone(),
            Resource::PromptCacheCoordinator => slots.prompt_cache_coordinator.clone(),
            Resource::Memory => slots.memory.clone(),
            Resource::MemoryService => slots.memory_service.clone(),
            Resource::MemoryManagement => slots.memory_management.clone(),
        }
    }
}

/// Write view handed to one family's staging: the baseline's field
/// assignments. Registering an owner is the rollback boundary — a family
/// registered here is disposed even when a later family (or the family's own
/// staging) fails.
pub struct StageSink<'a> {
    slots: &'a Arc<Mutex<Slots>>,
}
impl StageSink<'_> {
    /// `this.<family field> = handle` — the dispose owner. For the four
    /// bundle families this happens before their `stage()` can throw.
    pub fn register_owner(&mut self, family: Family, handle: Arc<dyn FamilyResource>) {
        let mut slots = lock(self.slots);
        match family {
            Family::ModelRuntime => slots.model_runtime = Some(handle),
            Family::PluginRuntime => slots.plugin_runtime = Some(handle),
            Family::RouterCustomRouters => slots.router_custom_routers = Some(handle),
            Family::RouterRuntime => slots.router_runtime = Some(handle),
            Family::LspService => slots.lsp_service = Some(handle),
            Family::ExecutionWorld => slots.execution_world_bundle = Some(handle),
            Family::Compaction => slots.compaction = Some(handle),
            Family::SessionTitleProvider => slots.session_title_provider = Some(handle),
            Family::McpProvider => slots.mcp_provider = Some(handle),
            Family::MemoryBundle => slots.memory_bundle = Some(handle),
            // Construct-only families own no rollback handle.
            Family::ContextStorage | Family::PromptCacheCoordinator => {}
        }
    }
    /// `this.resources.<field> = handle` — a published resource.
    pub fn register_resource(&mut self, resource: Resource, handle: Arc<dyn FamilyResource>) {
        let mut slots = lock(self.slots);
        match resource {
            Resource::Model => slots.model = Some(handle),
            Resource::ModelProviders => slots.model_providers = Some(handle),
            Resource::TokenAccounting => slots.token_accounting = Some(handle),
            Resource::Router => slots.router = Some(handle),
            Resource::PluginRuntime => slots.plugin_runtime_resource = Some(handle),
            Resource::RouterCustomRouters => slots.router_custom_routers_resource = Some(handle),
            Resource::Tools => slots.tools = Some(handle),
            Resource::ExecutionWorld => slots.execution_world = Some(handle),
            Resource::Interaction => slots.interaction = Some(handle),
            Resource::InstructionStorage => slots.instruction_storage = Some(handle),
            Resource::ToolResultSpill => slots.tool_result_spill = Some(handle),
            Resource::PromptCacheCoordinator => slots.prompt_cache_coordinator = Some(handle),
            Resource::Memory => slots.memory = Some(handle),
            Resource::MemoryService => slots.memory_service = Some(handle),
            Resource::MemoryManagement => slots.memory_management = Some(handle),
        }
    }
}

/// The one `RuntimeResources` group of a generation: the baseline's
/// `ProjectRuntimeResourcesBundle` and its `disposeResources` order. The
/// factory registers it before the first family step can fail, so a partial
/// build rolls back through it and a failed build publishes nothing.
#[derive(Clone)]
pub struct ProjectResources {
    slots: Arc<Mutex<Slots>>,
}
impl ProjectResources {
    fn new(
        project_root: PathBuf,
        pilot_home: PathBuf,
        builtin_skills_root: Option<PathBuf>,
        env: Env,
        now: Now,
        snapshot: Arc<JsValue>,
        profile: RuntimeProfile,
    ) -> Self {
        Self {
            slots: Arc::new(Mutex::new(Slots {
                project_root,
                pilot_home,
                builtin_skills_root,
                env,
                now,
                snapshot,
                profile,
                mcp_provider: None,
                plugin_runtime: None,
                router_custom_routers: None,
                compaction: None,
                session_title_provider: None,
                lsp_service: None,
                execution_world_bundle: None,
                router_runtime: None,
                model_runtime: None,
                memory_bundle: None,
                model: None,
                model_providers: None,
                token_accounting: None,
                router: None,
                plugin_runtime_resource: None,
                router_custom_routers_resource: None,
                tools: None,
                execution_world: None,
                interaction: None,
                instruction_storage: None,
                tool_result_spill: None,
                prompt_cache_coordinator: None,
                memory: None,
                memory_service: None,
                memory_management: None,
            })),
        }
    }
    /// Read view of this generation, before or after publication.
    pub fn context(&self) -> StageContext {
        StageContext {
            slots: Arc::clone(&self.slots),
        }
    }
    fn sink(&self) -> StageSink<'_> {
        StageSink { slots: &self.slots }
    }
}
impl RuntimeResources for ProjectResources {
    /// `disposeResources` (ProjectRuntimeResourcesBundle.ts:408-439): the mcp
    /// provider first, then the parallel set in array order, then the memory
    /// bundle. The baseline starts all eight parallel disposals together
    /// (`Promise.allSettled`, lines 416-425) and only awaits the memory bundle
    /// after every one of them settles, so a disposal that waits on a
    /// sibling's progress still sees that sibling start; failures are
    /// collected in the array order. Exactly one failure is rethrown as-is
    /// (line 435); two or more become the single `AggregateError("Failed to
    /// dispose project runtime resources.")` (lines 436-438). Context storage
    /// and the prompt-cache coordinator are never disposed, as in the
    /// baseline.
    fn dispose(&self) -> DisposeFuture<'_> {
        let slots = lock(&self.slots);
        let mcp_provider = slots.mcp_provider.clone();
        let parallel = (
            slots.plugin_runtime.clone(),
            slots.router_custom_routers.clone(),
            slots.compaction.clone(),
            slots.session_title_provider.clone(),
            slots.lsp_service.clone(),
            slots.execution_world_bundle.clone(),
            slots.router_runtime.clone(),
            slots.model_runtime.clone(),
        );
        let memory_bundle = slots.memory_bundle.clone();
        drop(slots);
        // The single group of a generation: `ResourceBundle::aggregate`
        // returns this error unchanged, so the group's own single- or
        // multi-failure shape reaches the caller without a second wrapper.
        Box::pin(async move {
            let (
                plugin_runtime,
                custom_routers,
                compaction,
                session_title,
                lsp,
                execution_world,
                router,
                model,
            ) = parallel;
            let mut failures = Vec::new();
            if let Err(error) = dispose_one(mcp_provider).await {
                failures.push(error);
            }
            // All eight start before any of them is awaited to completion,
            // like `Promise.allSettled`; `join!` polls them in array order
            // and reports their results in that order.
            let results = tokio::join!(
                dispose_one(plugin_runtime),
                dispose_one(custom_routers),
                dispose_one(compaction),
                dispose_one(session_title),
                dispose_one(lsp),
                dispose_one(execution_world),
                dispose_one(router),
                dispose_one(model)
            );
            for result in [
                results.0, results.1, results.2, results.3, results.4, results.5, results.6,
                results.7,
            ] {
                if let Err(error) = result {
                    failures.push(error);
                }
            }
            if let Err(error) = dispose_one(memory_bundle).await {
                failures.push(error);
            }
            RuntimeError::aggregate("Failed to dispose project runtime resources.", failures)
        })
    }
}

/// One family's disposal, when that family registered a rollback handle; the
/// baseline's `?? Promise.resolve()` arm for an absent family.
async fn dispose_one(handle: Option<Arc<dyn FamilyResource>>) -> RuntimeResult {
    match handle {
        Some(handle) => handle.dispose().await,
        None => Ok(()),
    }
}

/// The registry-facing factory options (the `ProjectRuntimeRegistryOptions`
/// that reach `buildRuntime`).
pub struct ProjectResourcesOptions {
    /// `options.pilotHome` — handed to the plugin and MCP families.
    pub pilot_home: PathBuf,
    /// Stands in for `process.cwd()` when resolving the Pilot home.
    pub cwd: PathBuf,
    /// `options.env` — the config read and every family see this.
    pub env: Env,
    /// `os.homedir()` seam for `resolvePilotHome`; `None` reads the process
    /// home directory.
    pub homedir: Option<PathBuf>,
    pub builtin_skills_root: Option<PathBuf>,
    /// `options.now`; the resource clock handed to the staged families. The
    /// config snapshot's `loadedAt` is the config read's own wall clock, as
    /// in the baseline.
    pub now: Now,
    /// `loadPilotConfig`'s `options.version ?? 1`.
    pub config_version: Option<f64>,
    pub runtime_profile_overrides: RuntimeProfileOverrides,
    pub families: Families,
}

/// The production `ProjectRuntimeFactory` (TS `buildRuntime`): one fresh
/// config read and staging sequence per registry generation, so config
/// reloads enter the new generation while the old one drains.
pub struct ProjectResourcesFactory {
    options: ProjectResourcesOptions,
    config: Arc<dyn ConfigSource>,
}
impl ProjectResourcesFactory {
    pub fn new(options: ProjectResourcesOptions) -> Self {
        let config = Arc::new(NativeConfigSource::new(
            options.cwd.clone(),
            options.env.clone(),
            options.homedir.clone(),
            options.config_version,
        ));
        Self { options, config }
    }

    /// A family whose option is absent but whose baseline default constructs
    /// a native resource: the default is not ported yet, so it reports
    /// explicitly instead of succeeding silently.
    fn stage_native_default(
        &self,
        source: Option<&Arc<dyn ResourceFamilySource>>,
        family: Family,
        detail: &'static str,
        context: &StageContext,
        sink: &mut StageSink<'_>,
    ) -> RuntimeResult {
        match source {
            Some(source) => source.stage(context, sink),
            None => unsupported(family, detail).stage(context, sink),
        }
    }
}
impl ProjectRuntimeFactory for ProjectResourcesFactory {
    /// `buildRuntime` + `ProjectRuntimeResourcesBundle.stage`
    /// (ProjectRuntimeResourcesBundle.ts:196-400), in baseline order. The
    /// group is registered before the first fallible family step, so every
    /// family that stages after a failure is rolled back through it.
    fn stage(&self, project_root: &Path, resources: &mut ResourceBundle) -> RuntimeResult {
        let snapshot = self.config.load(project_root)?;
        // `resolvePilotDeckRuntimeProfile({ autoElicitation, …, agent: snapshot.config.agent })`.
        let agent = snapshot
            .get("config")
            .and_then(|config| config.get("agent"))
            .cloned()
            .unwrap_or(JsValue::Undefined);
        let profile = RuntimeProfile::resolve(&agent, &self.options.runtime_profile_overrides);
        let group = ProjectResources::new(
            project_root.to_path_buf(),
            self.options.pilot_home.clone(),
            self.options.builtin_skills_root.clone(),
            self.options.env.clone(),
            Arc::clone(&self.options.now),
            snapshot,
            profile,
        );
        resources.add(group.clone());
        let context = group.context();
        let mut sink = group.sink();
        let families = &self.options.families;
        // `const modelRuntime = new ProjectModelRuntimeBundle({...});
        //  this.modelRuntime = modelRuntime; const {model, modelProviders}
        //  = modelRuntime.stage()` (lines 209-216): the source registers the
        // bundle before staging it.
        families.model_runtime.stage(&context, &mut sink)?;
        // `new PluginRuntime({...}); this.resources.pluginRuntime = ...`
        // (lines 218-225).
        families.plugins.stage(&context, &mut sink)?;
        // `this.options.routerSessionCustomRouterFactory?.()
        //  ?? createNativeRouterSessionCustomRouterPort()` (lines 226-228).
        self.stage_native_default(
            families.router_custom_routers.as_ref(),
            Family::RouterCustomRouters,
            "A6.9-pr3e createNativeRouterSessionCustomRouterPort",
            &context,
            &mut sink,
        )?;
        // `new ProjectRouterRuntimeBundle({...}); this.routerRuntime = ...;
        //  routerRuntime.stage()` (lines 230-250).
        families.router_runtime.stage(&context, &mut sink)?;
        // `this.options.lspServiceFactory?.({...}) ?? new LspService()`
        // (lines 252-256).
        self.stage_native_default(
            families.lsp.as_ref(),
            Family::LspService,
            "A6.9-pr3c LspService",
            &context,
            &mut sink,
        )?;
        // `new ProjectExecutionWorldBundle({...});
        //  this.executionWorldBundle = ...; executionWorldBundle.stage()`
        // (lines 258-275). The session-execution storage-root validation and
        // `createSessionExecution` closure (lines 276-332) are staged for
        // the session-execution family.
        families.execution_world.stage(&context, &mut sink)?;
        // `new ProjectContextStorageBundle(...).stage()` (line 334): no
        // rollback handle — the baseline holds no field before `stage()`.
        families.context_storage.stage(&context, &mut sink)?;
        // `this.options.compactionProviderFactory?.({...})` (lines 335-340):
        // absent by default.
        if let Some(source) = &families.compaction {
            source.stage(&context, &mut sink)?;
        }
        // `this.options.promptCacheCoordinatorFactory?.({...})`
        // (lines 341-345): absent by default, never disposed.
        if let Some(source) = &families.prompt_cache_coordinator {
            source.stage(&context, &mut sink)?;
        }
        // `this.options.sessionTitleProviderFactory?.({...})
        //  ?? createNativeSessionTitleProvider({...})` (lines 346-355).
        self.stage_native_default(
            families.session_title.as_ref(),
            Family::SessionTitleProvider,
            "A6.9-pr3d createNativeSessionTitleProvider",
            &context,
            &mut sink,
        )?;
        // `new ProjectMcpRuntimeProvider({...})` (lines 357-364): published
        // with the completed resources object (line 365).
        families.mcp.stage(&context, &mut sink)?;
        // `new ProjectMemoryBundle({...}); this.memoryBundle = ...;
        //  Object.assign(this.resources, memoryBundle.stage())`
        // (lines 387-398): the source registers the bundle before staging.
        families.memory.stage(&context, &mut sink)?;
        Ok(())
    }
}

#[cfg(test)]
#[path = "factory_tests.rs"]
mod tests;
