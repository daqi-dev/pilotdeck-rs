use super::{
    NativeTurnEnvironment, Options,
    assembly::{env_object, value},
};
use crate::{
    host::{
        HostError, HostFuture, ModuleError,
        modules::{
            CompositionSwitches, capability, model_port::RouterModelPortOptions,
            permission::PermissionDecision, tool_context::*,
        },
    },
    runtime::{
        hooks::Cancellation,
        interaction::GatewayInteractionCoordinator,
        mcp::{
            bridge::{
                CreateToolDefinitionsOptions, McpBridgeTool,
                create_mcp_tool_definitions_from_runtime,
            },
            protocol::McpStatus,
            registry::{
                McpDiagnosticSink, ProjectMcpRuntimeLease, ProjectMcpRuntimeProvider,
                SessionMcpRuntimeRegistration, SessionMcpRuntimeRegistry,
            },
            sdk::SdkMcpConfig,
        },
        permission::{self, DefaultPermissionContextOptions, PermissionContext, PermissionTool},
        project::{
            RuntimeError, RuntimeProfile, RuntimeProfileOverrides,
            interaction_permission::PermissionTurnOptions,
        },
        session::factory::SessionBinding,
        steer::MutationRecorder,
        tools::{SchedulerMode, ToolRegistry, ToolRuntime, builtin},
        turn::{host_factory::SessionPorts, interaction::InteractionTurnInput},
    },
    storage::recorder::Recorder,
};
use pilotdeck_protocol::{JsObject, JsString, JsValue, admission::ExecutionIdentity};
use pilotdeck_transport::{server::ExecutionClock, system_clock::SystemExecutionClock};
use std::{
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};

/// `ProjectSessionStorage.ts:413`: one session's tool-results directory under
/// the project root, with the path-safe session id. Shared by the per-turn
/// ports and the #897 session-composition preflight so the two cannot drift.
pub(super) fn session_tool_results_dir(project: &Path, session_id: &JsString) -> PathBuf {
    let dir = if crate::mutants::on("session-cwd-dir-layout") {
        project.join("tool-results")
    } else {
        project.join(".pilotdeck/tool-results")
    };
    if crate::mutants::on("session-cwd-dir-unsanitized") {
        dir.join(String::from_utf16_lossy(session_id.units()))
    } else {
        dir.join(crate::storage::sanitize_session_id(session_id))
    }
}

pub struct SessionTools {
    options: Options,
    env: JsObject,
    binding: SessionBinding,
    config: JsObject,
    interactions: Arc<GatewayInteractionCoordinator>,
    permission: Arc<Permissions>,
    lsp: Arc<super::families::DefaultLsp>,
    /// #1433: the session's composed MCP tools, registered into every turn's
    /// tool registry after the LSP entry (composed once at prepare, like
    /// `SessionMcpRuntimeBundle.compose`).
    mcp: SessionMcpTools,
    /// #1864 (b): re-read on every turn, like InProcessGateway.ts:1443 —
    /// the host-side permission context follows the stored selection so the
    /// same turn never runs one mode for the sidecar and another for native
    /// tool admission.
    permission_mode_source: Option<Arc<dyn Fn() -> Option<JsString> + Send + Sync>>,
}
impl SessionTools {
    pub fn new(
        options: &Options,
        env: &crate::config::Env,
        binding: SessionBinding,
        config: JsObject,
        interactions: Arc<GatewayInteractionCoordinator>,
        lsp: Arc<super::families::DefaultLsp>,
        mcp: SessionMcpTools,
    ) -> Result<Self, RuntimeError> {
        let context =
            permission::create_default_permission_context(DefaultPermissionContextOptions {
                cwd: options.project.to_string_lossy().into_owned().into(),
                can_prompt: Some(false),
                ..Default::default()
            });
        Ok(Self {
            options: options.clone(),
            env: env_object(env),
            binding,
            config,
            interactions,
            lsp,
            mcp,
            permission: Arc::new(Permissions(Mutex::new((context, None)))),
            permission_mode_source: None,
        })
    }

    /// #1864 (b): the live permission-mode source, consulted on every turn
    /// before the permission context is emitted. `None` keeps the context's
    /// current mode.
    pub fn with_permission_mode_source(
        mut self,
        source: Arc<dyn Fn() -> Option<JsString> + Send + Sync>,
    ) -> Self {
        self.permission_mode_source = Some(source);
        self
    }

    pub fn turn(
        &self,
        identity: &ExecutionIdentity,
        recorder: Arc<Recorder>,
        signal: Cancellation,
    ) -> SessionPorts {
        // #1864 (b): the effective mode — a non-empty
        // `managedPermissions.defaultMode` wins, else the stored selection —
        // is written back before the context is emitted, so `Permissions::
        // decide` (which clones the current value per call), the emitted
        // `permissionContext` and this turn's local `config.permissionMode`
        // (`tool_context` and `resolve_workspace_path` read that copy) all
        // agree with the sidecar's payload this turn. A `None` source leaves
        // the previous mode and the plan-round-trip stack below untouched.
        let (permission_context, permission_mode) = {
            let mut state = self.permission.0.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(mode) = permission::managed_default_mode(&self.config).or_else(|| {
                self.permission_mode_source
                    .as_ref()
                    .and_then(|source| source())
            }) {
                state.0.mode = crate::config::permission::PermissionMode::from(mode);
            }
            (state.0.to_value(), state.0.mode.as_js_string().clone())
        };
        let tools = native_tools(
            self.binding.context.allowed_tools.clone(),
            self.binding.context.disallowed_tools.clone(),
            self.lsp.clone(),
            &self.mcp,
        );
        let execution = Arc::new(ToolRuntime::new(
            tools,
            self.permission.clone(),
            SchedulerMode::Concurrent,
        ));
        let mut config = self.config.clone();
        config.insert("env".into(), JsValue::Object(self.env.clone()));
        config.insert("permissionMode".into(), JsValue::String(permission_mode));
        config.insert("permissionContext".into(), permission_context);
        let binding = TurnBinding {
            config: config.clone(),
            input: value([
                ("sessionId", JsValue::String(identity.session_id.clone())),
                ("turnId", JsValue::String(identity.turn_id.clone())),
            ]),
            checkpoint: self.binding.checkpoint.clone(),
            abort_signal: Some(Arc::new(signal.clone())),
            environment: Arc::new(NativeTurnEnvironment(self.env.clone())),
        };
        let tool_ports = ToolContextPorts {
            clock: Some(Arc::new(|| JsValue::String(SystemExecutionClock.now_iso()))),
            ..Default::default()
        };
        let sink = Durable {
            recorder,
            session: identity.session_id.clone(),
            turn: identity.turn_id.clone(),
        };
        let message_sink = sink.clone();
        SessionPorts {
            model: RouterModelPortOptions {
                project_path: Some(self.options.project.to_string_lossy().into_owned().into()),
                agent_model: self
                    .config
                    .get("provider")
                    .and_then(JsValue::as_string)
                    .zip(self.config.get("model").and_then(JsValue::as_string))
                    .map(|(p, m)| (p.clone(), m.clone())),
                agent_max_context_tokens: self.config.get("maxContextTokens").cloned(),
                ..RouterModelPortOptions::main_agent()
            },
            recorder: None,
            tool_results_dir: session_tool_results_dir(&self.options.project, &identity.session_id),
            context_runtime: None,
            context_identity: ContextRequestIdentity::new(binding.clone(), tool_ports.clone()),
            context_config: config,
            tool_execution: execution,
            tool_context: Arc::new(CapabilityContext {
                builder: ToolContextBuilder::new(binding, tool_ports, true),
                plan_todo: None,
            }),
            tool_observer: self.permission.clone(),
            plan_todo: None,
            turn: Default::default(),
            interaction: InteractionTurnInput {
                permission: PermissionTurnOptions {
                    profile: RuntimeProfile::resolve(
                        &JsValue::Undefined,
                        &RuntimeProfileOverrides::default(),
                    )
                    .interaction,
                    context: self
                        .permission
                        .0
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .0
                        .clone(),
                    rules: crate::config::permission::RuleSetHandle::empty(),
                    coordinator: self.interactions.clone(),
                    emit: Arc::new(|_| false),
                    timeout: Duration::from_secs(30),
                },
                hooks: vec![],
                contribution: None,
                base_env: Some(self.env.clone()),
                signal,
                steer_recorder: Some(Arc::new(sink)),
                record_message: Some(Arc::new(move |message| message_sink.message(message))),
                messages: vec![],
                switches: CompositionSwitches {
                    plan_todo: false,
                    elicitation_available: None,
                },
                turn_ports: Default::default(),
            },
        }
    }
}

struct LspTool {
    service: Arc<super::families::DefaultLsp>,
    fields: JsObject,
}
impl LspTool {
    fn new(service: Arc<super::families::DefaultLsp>) -> Self {
        Self { service, fields: value([
            ("name", "lsp".into()), ("kind", "filesystem".into()),
            ("description", "Query a language server for precise definitions, references, implementations, or hover information. Coordinates are one-based UTF-16 positions.".into()),
            ("inputSchema", pilotdeck_protocol::parse_json(r#"{"type":"object","required":["operation","file_path","line","character"],"additionalProperties":false,"properties":{"operation":{"type":"string","enum":["goToDefinition","findReferences","goToImplementation","hover"]},"file_path":{"type":"string"},"line":{"type":"integer"},"character":{"type":"integer"}}}"#).expect("native LSP schema")),
        ]) }
    }
}
impl capability::ToolDefinition for LspTool {
    fn fields(&self) -> &JsObject {
        &self.fields
    }
    fn is_read_only(&self, _: &JsValue) -> Result<JsValue, HostError> {
        Ok(JsValue::Bool(true))
    }
    fn is_concurrency_safe(&self, _: &JsValue) -> Result<JsValue, HostError> {
        Ok(JsValue::Bool(true))
    }
}
impl crate::runtime::tools::ExecutableTool for LspTool {
    fn validate_input(
        &self,
        input: JsValue,
        _: crate::runtime::tools::ToolRunContext,
    ) -> crate::runtime::tools::ToolFuture<Option<Vec<JsObject>>> {
        Box::pin(async move {
            for key in ["line", "character"] {
                if !input
                    .get(key)
                    .and_then(JsValue::as_number)
                    .is_some_and(|n| n.is_finite() && n.fract() == 0.0 && n >= 1.0)
                {
                    return Ok(Some(vec![value([
                        ("path", key.into()),
                        ("code", "invalid_schema".into()),
                        (
                            "message",
                            JsValue::String(
                                format!("{key} must be a positive one-based integer").into(),
                            ),
                        ),
                    ])]));
                }
            }
            if input
                .get("file_path")
                .and_then(JsValue::as_string)
                .is_none_or(|s| {
                    crate::metadata::js_trim(&String::from_utf16_lossy(s.units())).is_empty()
                })
            {
                return Ok(Some(vec![value([
                    ("path", "file_path".into()),
                    ("code", "invalid_schema".into()),
                    ("message", "file_path must not be empty".into()),
                ])]));
            }
            Ok(None)
        })
    }
    fn execute(
        &self,
        input: JsValue,
        context: crate::runtime::tools::ToolRunContext,
    ) -> crate::runtime::tools::ToolFuture<crate::runtime::tools::ToolOutput> {
        let service = self.service.clone();
        Box::pin(async move {
            let text = |v: Option<&JsValue>| {
                v.and_then(JsValue::as_string)
                    .map(|s| String::from_utf16_lossy(s.units()))
                    .unwrap_or_default()
            };
            let cwd = text(context.fields.get("cwd"));
            if crate::metadata::js_trim(&cwd).is_empty() {
                return Err(crate::runtime::tools::ToolError::new(
                    "LSP_WORKSPACE_REQUIRED",
                    "LSP tool requires a workspace cwd",
                ));
            }
            let result = service.query(
                &cwd,
                &text(input.get("file_path")),
                input
                    .get("line")
                    .and_then(JsValue::as_number)
                    .unwrap_or(f64::NAN)
                    - 1.0,
                input
                    .get("character")
                    .and_then(JsValue::as_number)
                    .unwrap_or(f64::NAN)
                    - 1.0,
            );
            match result {
                Err(error) => Err(crate::runtime::tools::ToolError::new(
                    error
                        .js_error()
                        .and_then(|e| e.code.clone())
                        .unwrap_or_else(|| "tool_execution_failed".into()),
                    error.message,
                )),
                Ok(_) => Err(crate::runtime::tools::ToolError::new(
                    "LSP_UNAVAILABLE",
                    "No native LSP provider is registered.",
                )),
            }
        })
    }
}

struct Permissions(
    Mutex<(
        PermissionContext,
        Option<crate::config::permission::PermissionMode>,
    )>,
);
struct Tool(
    Arc<dyn capability::ToolDefinition>,
    JsString,
    Option<JsString>,
);
impl PermissionTool for Tool {
    fn name(&self) -> &JsString {
        &self.1
    }
    fn kind(&self) -> Option<&JsString> {
        self.2.as_ref()
    }
    fn is_read_only(&self, input: &JsValue) -> Result<JsValue, HostError> {
        self.0.is_read_only(input)
    }
    fn check_permissions<'a>(
        &'a self,
        input: &'a JsValue,
        context: &'a PermissionContext,
    ) -> permission::PermissionFuture<'a, Option<JsValue>> {
        self.0.check_permissions(input, context)
    }
}
impl PermissionDecision for Permissions {
    fn decide(
        &self,
        tool: Arc<dyn capability::ToolDefinition>,
        input: JsValue,
        _: capability::RuntimeContext,
        id: JsString,
    ) -> HostFuture<JsValue> {
        let context = self.0.lock().unwrap_or_else(|e| e.into_inner()).0.clone();
        let name = tool
            .fields()
            .get("name")
            .and_then(JsValue::as_string)
            .cloned()
            .unwrap_or_default();
        let kind = tool
            .fields()
            .get("kind")
            .and_then(JsValue::as_string)
            .cloned();
        Box::pin(
            async move { permission::decide(&context, &Tool(tool, name, kind), &input, &id).await },
        )
    }
}
impl capability::CapabilityResultObserver for Permissions {
    fn on_capability_results(&self, results: Vec<JsObject>) -> HostFuture<()> {
        let mut state = self.0.lock().unwrap_or_else(|e| e.into_inner());
        for result in results {
            if result.get("type") != Some(&"success".into()) {
                continue;
            }
            let Some(mode) = result
                .get("data")
                .and_then(|v| v.get("requestedMode"))
                .filter(|v| permission::is_permission_mode(v))
                .and_then(JsValue::as_string)
            else {
                continue;
            };
            let mut mode = crate::config::permission::PermissionMode::from(mode.clone());
            if mode.as_js_string().equals_str("plan")
                && !state.0.mode.as_js_string().equals_str("plan")
            {
                state.1 = Some(state.0.mode.clone());
            } else if state.0.mode.as_js_string().equals_str("plan")
                && !mode.as_js_string().equals_str("plan")
                && let Some(previous) = state.1.take()
            {
                mode = previous;
            }
            state.0.mode = mode;
        }
        Box::pin(async { Ok(()) })
    }
}
#[derive(Clone)]
struct Durable {
    recorder: Arc<Recorder>,
    session: JsString,
    turn: JsString,
}
impl Durable {
    fn append(&self, turn: JsString, draft: JsObject) -> HostFuture<()> {
        let recorder = self.recorder.clone();
        let session = self.session.clone();
        Box::pin(async move {
            tokio::task::spawn_blocking(move || {
                recorder
                    .writer()
                    .record_session_event(&session, &turn, draft)
            })
            .await
            .map_err(|e| ModuleError::new(e.to_string()))?
            .map_err(|e| ModuleError::new(e.to_string()).into())
        })
    }
    fn message(&self, message: JsValue) -> HostFuture<()> {
        self.append(
            self.turn.clone(),
            value([("type", "durable_message".into()), ("message", message)]),
        )
    }
}
impl MutationRecorder for Durable {
    fn record(&self, turn: JsString, mutation: JsObject) -> HostFuture<()> {
        let mut draft = value([("type", "inbox_mutation".into())]);
        for (key, value) in mutation.entries() {
            draft.insert(key.clone(), value.clone());
        }
        self.append(turn, draft)
    }
}

pub(super) fn native_tools(
    allowed: Option<Vec<JsString>>,
    denied: Option<Vec<JsString>>,
    lsp: Arc<super::families::DefaultLsp>,
    mcp: &SessionMcpTools,
) -> ToolRegistry {
    let tools = ToolRegistry::new(
        None,
        Some(Arc::new(move |tool| {
            let Some(name) = tool.fields().get("name").and_then(JsValue::as_string) else {
                return false;
            };
            allowed.as_ref().is_none_or(|names| names.contains(name))
                && !denied.as_ref().is_some_and(|names| names.contains(name))
        })),
    );
    // The isolated entry advertises the native read-only family. Mutating
    // tools, subagents and integrations are not implied by an empty port.
    for tool in builtin::read_only_tools() {
        tools
            .register(tool)
            .expect("native builtin names are unique");
    }
    tools
        .register(Arc::new(LspTool::new(lsp)))
        .expect("native LSP name is unique");
    // #1433: the session's MCP tools join the same registry (and therefore
    // the same allowed/denied policy and permission layer) after the local
    // entries.
    mcp.register(&tools);
    tools
}

// ---------------------------------------------------------------------------
// #1433: session MCP tools in the turn composition
// ---------------------------------------------------------------------------

/// `getSdkMcpServers(sessionKey)` (ProjectRuntimeRegistry.ts:322-325): the
/// session's enabled servers as a `name -> config` map. The disabled entries
/// and the `{ config, enabled }` projection stay behind the read, so the
/// composition below only ever sees what a turn may use.
pub fn enabled_session_mcp_servers(mcp_config: &SdkMcpConfig, session_key: &JsString) -> JsObject {
    let mut servers = JsObject::new();
    for (name, value) in mcp_config.session_mcp_servers(&JsValue::String(session_key.clone())) {
        let enabled = value.get("enabled").cloned().unwrap_or(JsValue::Undefined);
        if !crate::runtime::mcp::sdk::js_truthy(&enabled) {
            continue;
        }
        servers.insert(
            name,
            value.get("config").cloned().unwrap_or(JsValue::Undefined),
        );
    }
    servers
}

/// #1433 (`SessionMcpRuntimeBundle.compose`, the tool half): one session's
/// composed MCP tool set. `shared` holds the shared-runtime lease's tools —
/// registration keeps existing names; `per_session` holds the dedicated
/// runtime's tools — registration replaces same-name entries
/// (SessionMcpRuntimeBundle.ts:61-64, 138-144).
#[derive(Default)]
pub struct SessionMcpTools {
    shared: Vec<Arc<McpBridgeTool>>,
    per_session: Vec<Arc<McpBridgeTool>>,
}

impl SessionMcpTools {
    /// The empty composition: no session servers, nothing registered.
    pub fn empty() -> Self {
        Self::default()
    }

    pub fn shared(&self) -> &[Arc<McpBridgeTool>] {
        &self.shared
    }

    pub fn per_session(&self) -> &[Arc<McpBridgeTool>] {
        &self.per_session
    }

    pub fn is_empty(&self) -> bool {
        self.shared.is_empty() && self.per_session.is_empty()
    }

    /// The registration tail of `compose`: shared-lease tools keep existing
    /// names, per-session tools replace them.
    pub fn register(&self, tools: &ToolRegistry) {
        for tool in &self.shared {
            let name = tool.spec().wire_name.clone();
            if !tools.has(&name) {
                tools
                    .register(tool.clone())
                    .expect("#1433 shared session MCP tool name is unique");
            }
        }
        for tool in &self.per_session {
            let name = tool.spec().wire_name.clone();
            if tools.has(&name) {
                tools
                    .replace(tool.clone())
                    .expect("#1433 per-session MCP tool replaces its own name");
            } else {
                tools
                    .register(tool.clone())
                    .expect("#1433 per-session MCP tool name is unique");
            }
        }
    }
}

/// #1433 (`GatewaySessionResourceLeaseBundle`, the release half): the
/// close-time obligations of one composed session — the per-session runtime
/// registration first, then the shared-runtime lease (the reverse of the
/// composition order).
#[derive(Default)]
pub struct SessionMcpRelease {
    per_session: Option<SessionMcpRuntimeRegistration>,
    shared: Option<ProjectMcpRuntimeLease>,
}
impl SessionMcpRelease {
    /// Release both obligations. The first failure still releases the second,
    /// and both failures surface together.
    pub async fn release(self) -> Result<(), RuntimeError> {
        let mut failures = Vec::new();
        if let Some(registration) = self.per_session
            && let Err(error) = registration.dispose().await
        {
            failures.push(error);
        }
        if let Some(lease) = self.shared
            && let Err(error) = lease.release().await
        {
            failures.push(error);
        }
        match failures.len() {
            0 => Ok(()),
            1 => Err(failures
                .pop()
                .expect("#1433 exactly one MCP release failure")),
            _ => Err(RuntimeError {
                message: format!(
                    "Failed to release session MCP resources: {}",
                    failures
                        .iter()
                        .map(|error| error.message.clone())
                        .collect::<Vec<_>>()
                        .join("; ")
                ),
                causes: failures,
                js_error: None,
            }),
        }
    }
}

/// #1433: inputs of [`session_mcp_tools`] (`SessionMcpRuntimeBundleOptions`).
pub struct SessionMcpToolsOptions<'a> {
    /// The SDK-owned session server table (`set_mcp_servers` commits here).
    pub mcp_config: &'a SdkMcpConfig,
    pub provider: &'a ProjectMcpRuntimeProvider,
    /// The session factory's per-session registry; gateway disposal drains
    /// it (`ProjectSessionFactory.mcp`).
    pub per_session_runtimes: &'a SessionMcpRuntimeRegistry,
    pub session_key: &'a JsString,
    /// `runtime.snapshot.config.gateway.maxPerSessionMcpInstances ?? 5` —
    /// the snapshot's already-normalized f64 (finite, floored at 1 by
    /// `config/snapshot.rs`), compared against the registry size without
    /// truncation: the TS check is `size >= maxPerSessionInstances`
    /// (SessionMcpRuntimeBundle.ts:70), where a cap of 2.5 still admits the
    /// third registration (`2 >= 2.5` is false).
    pub max_per_session_instances: f64,
    /// `onDiagnostic`; production writes to stderr.
    pub on_diagnostic: Option<McpDiagnosticSink>,
}

/// #1433: `SessionMcpRuntimeBundle.compose()` — build one session's MCP tool
/// set from the enabled session server table. This is the production read
/// face of `SdkMcpConfig::session_mcp_servers` (#1433): shared servers lease
/// the project runtime (`acquireSharedRuntime`), per-session stdio servers
/// start a dedicated runtime under the session registry. Per-session start
/// failures degrade to diagnostics (the session keeps the shared tools); only
/// a non-active provider or registry fails the caller.
pub async fn session_mcp_tools(
    options: SessionMcpToolsOptions<'_>,
) -> Result<(SessionMcpTools, SessionMcpRelease), RuntimeError> {
    let session_key = String::from_utf16_lossy(options.session_key.units());
    let servers = enabled_session_mcp_servers(options.mcp_config, options.session_key);
    let diagnostic = |message: String, error: Option<RuntimeError>| {
        if let Some(sink) = &options.on_diagnostic {
            sink(message, error);
        }
    };
    // `compose()` acquires the shared runtime before anything per-session;
    // the lease travels with the release half whatever happens next.
    let shared_lease = options.provider.acquire_shared_runtime(&servers).await?;
    let tools = SessionMcpTools {
        shared: shared_lease.tools().to_vec(),
        per_session: Vec::new(),
    };
    let mut release = SessionMcpRelease {
        per_session: None,
        shared: Some(shared_lease),
    };
    let Some(specs) = options.provider.per_session_server_specs(&servers, false)? else {
        return Ok((tools, release));
    };
    let runtime = options.provider.create_runtime(specs.clone());
    // #2442: the cap comparison and the registration are one critical section
    // (`register_below_cap`) — the old two-lock check-then-register let two
    // concurrent prepares on the factory-level shared registry each read a
    // below-cap size and both register past the cap (TS has no interleaving
    // point between `perSessionRuntimes.size >= maxPerSessionInstances`
    // (SessionMcpRuntimeBundle.ts:70) and the insert (:119-125)). Building
    // the runtime first is inert: the construction spawns nothing, and the
    // start stays below the registration.
    let Some(registration) = options.per_session_runtimes.register_below_cap(
        &session_key,
        runtime.clone(),
        options.max_per_session_instances,
    )?
    else {
        diagnostic(
            format!(
                "Per-session MCP limit reached ({}). Session {session_key} will not start: {}.",
                options.max_per_session_instances,
                specs
                    .iter()
                    .map(|spec| spec.id.as_str())
                    .collect::<Vec<_>>()
                    .join(", "),
            ),
            None,
        );
        return Ok((tools, release));
    };
    // Registration precedes the start (SessionMcpRuntimeBundle.ts:119-125):
    // even a failed start is disposed at close, never leaked.
    release.per_session = Some(registration);
    match runtime.start().await {
        Ok(statuses) => {
            for status in statuses {
                if status.status == McpStatus::Error {
                    diagnostic(
                        format!(
                            "{} (server={}, session={session_key}): {}",
                            if status.server_id == "funasr" {
                                "ASR unavailable"
                            } else {
                                "Per-session MCP unavailable"
                            },
                            status.server_id,
                            status.error.as_deref().unwrap_or("unknown error"),
                        ),
                        None,
                    );
                }
            }
        }
        Err(error) => {
            diagnostic(
                format!("Per-session MCP startup failed for {session_key}"),
                Some(error),
            );
            return Ok((tools, release));
        }
    }
    match create_mcp_tool_definitions_from_runtime(runtime, CreateToolDefinitionsOptions::default())
        .await
    {
        Ok(per_session) => Ok((
            SessionMcpTools {
                per_session,
                ..tools
            },
            release,
        )),
        Err(error) => {
            diagnostic(
                format!("Per-session MCP startup failed for {session_key}"),
                Some(error),
            );
            Ok((tools, release))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::mcp::{
        client::{McpCallOptions, McpClientError, McpToolCallResult},
        protocol::{McpServerSpec, McpToolSpec},
        registry::{BuiltinMcpPaths, ProjectMcpRuntimeProviderOptions},
        runtime::{McpClientStatusEntry, McpFuture, McpRuntimePort, McpRuntimeServerInfo},
        sdk::{
            SdkMcpConfigOptions, SdkMcpEnvironment, SdkMcpError, SdkMcpFuture, SdkMcpProject,
            SdkMcpProjectResolver, SdkMcpSessionPlugins, SystemEnvironment,
        },
    };

    fn wire_name(tool: &McpBridgeTool) -> String {
        String::from_utf16_lossy(tool.spec().wire_name.units())
    }

    // ── SdkMcpConfig harness (the sdk_mcp.rs external-test shape, in-crate)

    struct EmptyProject;
    impl SdkMcpProject for EmptyProject {
        fn project_root(&self) -> JsString {
            "/pdrs-fixture/project".into()
        }
        fn refresh<'a>(&'a self) -> SdkMcpFuture<'a, Result<(), SdkMcpError>> {
            Box::pin(async { Ok(()) })
        }
        fn snapshot<'a>(
            &'a self,
        ) -> SdkMcpFuture<'a, Vec<Arc<crate::runtime::plugins::LoadedPlugin>>> {
            Box::pin(async { Vec::new() })
        }
    }
    struct StaticResolver;
    impl SdkMcpProjectResolver for StaticResolver {
        fn resolve<'a>(
            &'a self,
            _: Option<&'a JsString>,
        ) -> SdkMcpFuture<'a, Result<Arc<dyn SdkMcpProject>, SdkMcpError>> {
            let project: Arc<dyn SdkMcpProject> = Arc::new(EmptyProject);
            Box::pin(async { Ok(project) })
        }
    }
    struct NoPlugins;
    impl SdkMcpSessionPlugins for NoPlugins {
        fn plugins(&self, _: &JsValue) -> Vec<Arc<crate::runtime::plugins::LoadedPlugin>> {
            Vec::new()
        }
    }
    struct FixtureEnvironment;
    impl SdkMcpEnvironment for FixtureEnvironment {
        fn env(&self, _: &JsString) -> Option<JsString> {
            None
        }
        fn home(&self) -> JsString {
            "/pdrs-fixture/home".into()
        }
    }

    fn test_config() -> Arc<SdkMcpConfig> {
        Arc::new(SdkMcpConfig::new(SdkMcpConfigOptions {
            resolver: Arc::new(StaticResolver),
            close_session: Arc::new(|_, _| Box::pin(async { Ok(()) })),
            session_plugins: Arc::new(NoPlugins),
            permission_rule_sets: Arc::new(
                crate::config::permission::GatewaySessionPermissionRuleSetRegistry::new(),
            ),
            environment: Arc::new(FixtureEnvironment),
            projects: crate::runtime::sdk_session::SdkSessionProjects::shared(),
        }))
    }

    /// `setMcpServersForSdk` input: `(name, per_session)` stdio servers.
    fn set_input(session_key: &str, servers: &[(&str, bool)]) -> JsValue {
        let mut map = JsObject::new();
        for (name, per_session) in servers {
            let mut config = JsObject::new();
            config.insert("type".into(), JsValue::String("stdio".into()));
            config.insert("command".into(), JsValue::String("node".into()));
            if *per_session {
                config.insert("perSession".into(), JsValue::Bool(true));
            }
            map.insert((*name).into(), JsValue::Object(config));
        }
        let mut input = JsObject::new();
        input.insert("sessionKey".into(), JsValue::String(session_key.into()));
        input.insert("servers".into(), JsValue::Object(map));
        JsValue::Object(input)
    }

    fn toggle_input(session_key: &str, server_name: &str, enabled: bool) -> JsValue {
        let mut input = JsObject::new();
        input.insert("sessionKey".into(), JsValue::String(session_key.into()));
        input.insert("serverName".into(), JsValue::String(server_name.into()));
        input.insert("enabled".into(), JsValue::Bool(enabled));
        JsValue::Object(input)
    }

    // ── provider fake (`create_runtime` seam, like the TS bundle's factory)

    struct FakeMcpRuntime {
        label: &'static str,
        tools: Vec<McpToolSpec>,
        start_fails: bool,
        events: Arc<Mutex<Vec<String>>>,
    }
    impl crate::runtime::mcp::runtime::SessionMcpRuntime for FakeMcpRuntime {
        fn stop(&self) -> McpFuture<'_, Result<(), RuntimeError>> {
            let events = self.events.clone();
            let label = self.label;
            Box::pin(async move {
                events
                    .lock()
                    .expect("#1433 events lock")
                    .push(format!("stop:{label}"));
                Ok(())
            })
        }
    }
    impl McpRuntimePort for FakeMcpRuntime {
        fn start(&self) -> McpFuture<'_, Result<Vec<McpClientStatusEntry>, RuntimeError>> {
            if self.start_fails {
                return Box::pin(async { Err(RuntimeError::new("injected start failure")) });
            }
            let statuses = self
                .tools
                .iter()
                .map(|tool| McpClientStatusEntry {
                    server_id: String::from_utf16_lossy(tool.server_id.units()),
                    status: McpStatus::Ready,
                    error: None,
                })
                .collect();
            Box::pin(async { Ok(statuses) })
        }
        fn list_all_tools(&self) -> McpFuture<'_, Result<Vec<McpToolSpec>, RuntimeError>> {
            let tools = self.tools.clone();
            Box::pin(async { Ok(tools) })
        }
        fn server_info(&self, _: &str) -> Option<McpRuntimeServerInfo> {
            Some(McpRuntimeServerInfo {
                transport: "stdio",
                cwd: None,
            })
        }
        fn call_tool<'a>(
            &'a self,
            _: &'a str,
            _: &'a JsString,
            _: &'a JsValue,
            _: McpCallOptions,
        ) -> McpFuture<'a, Result<McpToolCallResult, McpClientError>> {
            Box::pin(async {
                Err(McpClientError::new(
                    "fake",
                    None,
                    "the fake runtime does not call tools",
                ))
            })
        }
    }

    /// #1433 fix-r1 (review P1-2) probe runtime: `stop()` parks on a
    /// one-shot gate, standing in for a slow child shutdown. The label is
    /// pushed before the gate is awaited, so a test can wait for the stop to
    /// have parked.
    struct GatedRuntime {
        label: &'static str,
        gate: Mutex<Option<tokio::sync::oneshot::Receiver<()>>>,
        events: Arc<Mutex<Vec<String>>>,
    }
    impl GatedRuntime {
        fn new(
            label: &'static str,
            gate: tokio::sync::oneshot::Receiver<()>,
            events: Arc<Mutex<Vec<String>>>,
        ) -> Self {
            Self {
                label,
                gate: Mutex::new(Some(gate)),
                events,
            }
        }
    }
    impl crate::runtime::mcp::runtime::SessionMcpRuntime for GatedRuntime {
        fn stop(&self) -> McpFuture<'_, Result<(), RuntimeError>> {
            let gate = self.gate.lock().expect("#1433 gate lock").take();
            let events = self.events.clone();
            let label = self.label;
            Box::pin(async move {
                events
                    .lock()
                    .expect("#1433 events lock")
                    .push(format!("stop:{label}"));
                if let Some(receiver) = gate {
                    let _ = receiver.await;
                }
                Ok(())
            })
        }
    }
    impl McpRuntimePort for GatedRuntime {
        fn start(&self) -> McpFuture<'_, Result<Vec<McpClientStatusEntry>, RuntimeError>> {
            Box::pin(async { Ok(Vec::new()) })
        }
        fn list_all_tools(&self) -> McpFuture<'_, Result<Vec<McpToolSpec>, RuntimeError>> {
            Box::pin(async { Ok(Vec::new()) })
        }
        fn server_info(&self, _: &str) -> Option<McpRuntimeServerInfo> {
            None
        }
        fn call_tool<'a>(
            &'a self,
            _: &'a str,
            _: &'a JsString,
            _: &'a JsValue,
            _: McpCallOptions,
        ) -> McpFuture<'a, Result<McpToolCallResult, McpClientError>> {
            Box::pin(async {
                Err(McpClientError::new(
                    "gated",
                    None,
                    "the gated runtime does not call tools",
                ))
            })
        }
    }

    fn fake_provider(
        events: Arc<Mutex<Vec<String>>>,
        start_fails: bool,
    ) -> Arc<ProjectMcpRuntimeProvider> {
        let create_runtime = Arc::new(move |specs: Vec<McpServerSpec>| {
            let per_session = specs.iter().any(|spec| {
                matches!(
                    spec.transport,
                    crate::runtime::mcp::protocol::McpTransportSpec::Stdio {
                        per_session: true,
                        ..
                    }
                )
            });
            let runtime: Arc<dyn McpRuntimePort> = Arc::new(FakeMcpRuntime {
                label: if per_session { "per-session" } else { "shared" },
                tools: specs
                    .iter()
                    .map(|spec| McpToolSpec {
                        server_id: spec.id.as_str().into(),
                        tool_name: "echo".into(),
                        wire_name: format!("mcp__{}__echo", spec.id).into(),
                        description: "fake echo".into(),
                        input_schema: pilotdeck_protocol::parse_json("{}")
                            .expect("#1433 fake tool schema"),
                        annotations: None,
                        meta: None,
                    })
                    .collect(),
                start_fails,
                events: events.clone(),
            });
            runtime
        });
        Arc::new(ProjectMcpRuntimeProvider::new(
            ProjectMcpRuntimeProviderOptions {
                project_root: "/pdrs-fixture/project".into(),
                pilot_home: "/pdrs-fixture/home".into(),
                create_runtime: Some(create_runtime),
                proxy: None,
                on_diagnostic: None,
                environment: Arc::new(SystemEnvironment),
                builtin_paths: BuiltinMcpPaths::default(),
            },
        ))
    }

    async fn compose(
        config: &Arc<SdkMcpConfig>,
        provider: &Arc<ProjectMcpRuntimeProvider>,
        runtimes: &SessionMcpRuntimeRegistry,
        session_key: &str,
        max_per_session_instances: f64,
        on_diagnostic: Option<crate::runtime::mcp::registry::McpDiagnosticSink>,
    ) -> Result<(SessionMcpTools, SessionMcpRelease), RuntimeError> {
        session_mcp_tools(SessionMcpToolsOptions {
            mcp_config: config,
            provider,
            per_session_runtimes: runtimes,
            session_key: &JsString::from(session_key),
            max_per_session_instances,
            on_diagnostic,
        })
        .await
    }

    /// `getSdkMcpServers` (#1433): only enabled entries, projected to their
    /// configs, in insertion order.
    #[tokio::test]
    async fn enabled_session_mcp_servers_projects_only_enabled_configs() {
        let config = test_config();
        config
            .set_mcp_servers(&set_input("s", &[("a", false), ("b", false)]))
            .await
            .expect("#1433 fixture servers commit");
        config
            .toggle_mcp_server(&toggle_input("s", "b", false))
            .await
            .expect("#1433 fixture server b disabled");
        let servers = enabled_session_mcp_servers(&config, &"s".into());
        let projected = pilotdeck_protocol::stringify_json(&JsValue::Object(servers))
            .expect("#1433 servers serialize")
            .expect("#1433 servers are not undefined");
        crate::negative_assert_eq!(
            projected,
            r#"{"a":{"type":"stdio","command":"node"}}"#,
            "#1433 enabled session servers project to their configs"
        );
    }

    /// `SessionMcpRuntimeBundle.compose` (#1433): shared servers lease the
    /// project runtime, per-session stdio servers start under the session
    /// registry, and the release disposes the registration.
    #[tokio::test]
    async fn session_mcp_tools_composes_shared_and_per_session_tools() {
        let config = test_config();
        config
            .set_mcp_servers(&set_input(
                "s",
                &[("shared-echo", false), ("private-echo", true)],
            ))
            .await
            .expect("#1433 fixture servers commit");
        let provider = fake_provider(Arc::new(Mutex::new(Vec::new())), false);
        let runtimes = SessionMcpRuntimeRegistry::new();
        let (tools, release) = crate::test_negative_receipt::expect_ok(
            compose(&config, &provider, &runtimes, "s", 5.0, None).await,
            "#1433 composition succeeds",
        );
        crate::negative_assert!(
            tools
                .shared()
                .iter()
                .any(|tool| wire_name(tool) == "mcp__shared-echo__echo"),
            "#1433 shared session server tool composed"
        );
        crate::negative_assert!(
            tools
                .per_session()
                .iter()
                .any(|tool| wire_name(tool) == "mcp__private-echo__echo"),
            "#1433 per-session server tool composed"
        );
        crate::negative_assert_eq!(runtimes.size(), 1, "#1433 per-session runtime registered");
        release.release().await.expect("#1433 composition release");
        crate::negative_assert_eq!(
            runtimes.size(),
            0,
            "#1433 per-session runtime disposed on release"
        );
    }

    /// `GatewaySessionResourceLeaseBundle` (#1433): the per-session runtime
    /// stops before the shared lease on release.
    #[tokio::test]
    async fn session_mcp_release_stops_per_session_before_shared() {
        let config = test_config();
        config
            .set_mcp_servers(&set_input(
                "s",
                &[("shared-echo", false), ("private-echo", true)],
            ))
            .await
            .expect("#1433 fixture servers commit");
        let events: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let provider = fake_provider(events.clone(), false);
        let (_, release) = crate::test_negative_receipt::expect_ok(
            compose(
                &config,
                &provider,
                &SessionMcpRuntimeRegistry::new(),
                "s",
                5.0,
                None,
            )
            .await,
            "#1433 composition succeeds",
        );
        release.release().await.expect("#1433 composition release");
        crate::negative_assert_eq!(
            events.lock().expect("#1433 events lock").clone(),
            vec!["stop:per-session".to_owned(), "stop:shared".to_owned()],
            "#1433 release order: per-session MCP before shared MCP"
        );
    }

    /// #1433 fix-r1 (review P1-2): a slow per-session stop (waiting on its
    /// child) must not hold any lock the concurrent registry and provider
    /// disposal needs. The `released: Mutex<bool>` guards, the registry's
    /// inner lock and the provider's state lock are all released before their
    /// awaited stops, so a concurrent `dispose()` reaches its own settle wait
    /// instead of parking on a mutex held across the release's await.
    /// #2444: the shared half is observed in its own window — the shared
    /// gate opens only after the release has parked on it and the provider
    /// dispose is waiting at its settle, and a probe takes the provider
    /// state lock while that awaited stop is really in flight.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn session_mcp_release_stays_concurrent_with_registry_and_provider_dispose() {
        let events: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let runtimes = SessionMcpRuntimeRegistry::new();
        let (open_per_session, per_session_gate) = tokio::sync::oneshot::channel::<()>();
        let registration = runtimes
            .register(
                "s",
                Arc::new(GatedRuntime::new(
                    "per-session",
                    per_session_gate,
                    events.clone(),
                )),
            )
            .expect("#1433 gated per-session registration");
        runtimes
            .register(
                "t",
                Arc::new(FakeMcpRuntime {
                    label: "draining",
                    tools: Vec::new(),
                    start_fails: false,
                    events: events.clone(),
                }),
            )
            .expect("#1433 concurrent per-session registration");
        let (open_shared, shared_gate) = tokio::sync::oneshot::channel::<()>();
        // The factory must stay `Fn`: the one-shot receiver leaves through an
        // interior-mutability slot, not by moving out of the capture.
        let shared_gate_slot = Arc::new(Mutex::new(Some(shared_gate)));
        let gated_shared = {
            let slot = shared_gate_slot.clone();
            let events = events.clone();
            move |_specs: Vec<McpServerSpec>| {
                Arc::new(GatedRuntime::new(
                    "shared",
                    slot.lock()
                        .expect("#1433 gate slot lock")
                        .take()
                        .expect("#1433 exactly one shared generation"),
                    events.clone(),
                )) as Arc<dyn McpRuntimePort>
            }
        };
        let provider = Arc::new(ProjectMcpRuntimeProvider::new(
            ProjectMcpRuntimeProviderOptions {
                project_root: "/pdrs-fixture/project".into(),
                pilot_home: "/pdrs-fixture/home".into(),
                create_runtime: Some(Arc::new(gated_shared)),
                proxy: None,
                on_diagnostic: None,
                environment: Arc::new(SystemEnvironment),
                builtin_paths: BuiltinMcpPaths::default(),
            },
        ));
        let config = test_config();
        config
            .set_mcp_servers(&set_input("s", &[("shared-echo", false)]))
            .await
            .expect("#1433 fixture servers commit");
        let servers = enabled_session_mcp_servers(&config, &"s".into());
        let lease = crate::test_negative_receipt::expect_ok(
            provider.acquire_shared_runtime(&servers).await,
            "#1433 gated shared lease",
        );
        let release = SessionMcpRelease {
            per_session: Some(registration),
            shared: Some(lease),
        };
        let release_task = tokio::spawn(async move { release.release().await });
        // Wait until the release is parked on the gated per-session stop: the
        // spawned stop pushes its label before awaiting the gate.
        let mut parked = false;
        for _ in 0..400 {
            if events
                .lock()
                .expect("#1433 events lock")
                .iter()
                .any(|event| event == "stop:per-session")
            {
                parked = true;
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
        crate::negative_assert!(
            parked,
            "#1433 the release parks on the gated per-session stop"
        );
        // While the release is parked, the locks a concurrent dispose needs
        // stay available (the probes run as their own tasks, so a held lock
        // would surface as a timeout, not a blocked worker).
        let probe = {
            let runtimes = runtimes.clone();
            let provider = provider.clone();
            tokio::spawn(async move { (runtimes.size(), provider.shared_generation_count()) })
        };
        let (size, generations) = tokio::time::timeout(std::time::Duration::from_secs(2), probe)
            .await
            .expect("#1433 the registry and provider stay lockable during release")
            .expect("#1433 the lockability probe joins");
        crate::negative_assert_eq!(
            size,
            1,
            "#1433 the parked release has unlinked its own entry already"
        );
        crate::negative_assert_eq!(
            generations,
            1,
            "#1433 the parked release keeps the shared entry for its lease"
        );
        // `registry.dispose()` needs no gate: the parked release already
        // unlinked its own entry under the inner lock, so only the ungated
        // "draining" entry remains. Awaiting it before any gate opens makes
        // the "not blocked" claim real (#2444: awaiting it after the gates
        // opened proved nothing).
        let registry_dispose = {
            let runtimes = runtimes.clone();
            tokio::spawn(async move { runtimes.dispose().await })
        };
        let registry_result =
            tokio::time::timeout(std::time::Duration::from_secs(2), registry_dispose)
                .await
                .expect("#1433 registry dispose is not blocked by the parked release")
                .expect("#1433 the registry dispose task joins");
        crate::test_negative_receipt::expect_ok(registry_result, "#1433 registry dispose returns");
        let provider_dispose = {
            let provider = provider.clone();
            tokio::spawn(async move { provider.dispose().await })
        };
        open_per_session
            .send(())
            .expect("#1433 open the per-session stop gate");
        // #2444: wait until the release reaches the gated shared stop — the
        // shared entry's stop closure pushes its label before awaiting the
        // gate — so the provider lock is probed while that await is really
        // in flight, not before the shared half has even started (the empty
        // window the back-to-back gate sends used to leave).
        let mut shared_parked = false;
        for _ in 0..400 {
            if events
                .lock()
                .expect("#1433 events lock")
                .iter()
                .any(|event| event == "stop:shared")
            {
                shared_parked = true;
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
        crate::negative_assert!(
            shared_parked,
            "#1433 the release parks on the gated shared stop"
        );
        // The provider half of the invariant, observed inside the window
        // where the shared stop is parked on its gate: the state lock must
        // be free across that await (the probe runs as its own task, so a
        // held lock surfaces as a timeout, not a blocked worker). This
        // bound assertion runs before the `!is_finished` probe below: an
        // early state unlink would also let `provider.dispose()` settle,
        // and dying there first would report a plain failure instead of
        // this test's bound receipt.
        let provider_probe = {
            let provider = provider.clone();
            tokio::spawn(async move { provider.shared_generation_count() })
        };
        let generations_after_park =
            tokio::time::timeout(std::time::Duration::from_secs(2), provider_probe)
                .await
                .expect("#1433 the provider state lock is free across the awaited shared stop")
                .expect("#1433 the provider probe task joins");
        crate::negative_assert_eq!(
            generations_after_park,
            1,
            "#1433 the parked shared stop keeps its entry until it finishes"
        );
        crate::negative_assert!(
            !provider_dispose.is_finished(),
            "#1433 the provider dispose waits for the gated shared stop"
        );
        open_shared
            .send(())
            .expect("#1433 open the shared stop gate");
        let released = tokio::time::timeout(std::time::Duration::from_secs(2), release_task)
            .await
            .expect("#1433 the release finishes once the gates open")
            .expect("#1433 the release task joins");
        crate::test_negative_receipt::expect_ok(released, "#1433 gated release returns");
        let provider_result =
            tokio::time::timeout(std::time::Duration::from_secs(2), provider_dispose)
                .await
                .expect("#1433 provider dispose is not blocked by the parked release")
                .expect("#1433 the provider dispose task joins");
        crate::test_negative_receipt::expect_ok(provider_result, "#1433 provider dispose returns");
        crate::negative_assert_eq!(
            events
                .lock()
                .expect("#1433 events lock")
                .iter()
                .filter(|event| event.as_str() == "stop:per-session")
                .count(),
            1,
            "#1433 the per-session stop ran exactly once"
        );
        crate::negative_assert_eq!(
            events
                .lock()
                .expect("#1433 events lock")
                .iter()
                .filter(|event| event.as_str() == "stop:shared")
                .count(),
            1,
            "#1433 the shared stop ran exactly once"
        );
    }

    /// `compose` (#1433): a disabled server never reaches the composition.
    #[tokio::test]
    async fn session_mcp_tools_skips_disabled_servers() {
        let config = test_config();
        config
            .set_mcp_servers(&set_input("s", &[("a", true)]))
            .await
            .expect("#1433 fixture servers commit");
        config
            .toggle_mcp_server(&toggle_input("s", "a", false))
            .await
            .expect("#1433 fixture server disabled");
        let provider = fake_provider(Arc::new(Mutex::new(Vec::new())), false);
        let (tools, release) = crate::test_negative_receipt::expect_ok(
            compose(
                &config,
                &provider,
                &SessionMcpRuntimeRegistry::new(),
                "s",
                5.0,
                None,
            )
            .await,
            "#1433 composition succeeds",
        );
        crate::negative_assert!(
            tools.is_empty(),
            "#1433 a disabled session server composes no tools"
        );
        release.release().await.expect("#1433 composition release");
    }

    /// `compose` (#1433): the per-session limit degrades to a diagnostic and
    /// keeps the shared tools; no runtime is registered past the limit.
    #[tokio::test]
    async fn session_mcp_tools_respects_the_per_session_limit() {
        let config = test_config();
        config
            .set_mcp_servers(&set_input(
                "s",
                &[("shared-echo", false), ("private-echo", true)],
            ))
            .await
            .expect("#1433 fixture servers commit");
        let provider = fake_provider(Arc::new(Mutex::new(Vec::new())), false);
        let runtimes = SessionMcpRuntimeRegistry::new();
        runtimes
            .register(
                "other",
                Arc::new(FakeMcpRuntime {
                    label: "per-session",
                    tools: Vec::new(),
                    start_fails: false,
                    events: Arc::new(Mutex::new(Vec::new())),
                }),
            )
            .expect("#1433 pre-existing per-session runtime");
        let diagnostics: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let sink = diagnostics.clone();
        let (tools, release) = crate::test_negative_receipt::expect_ok(
            compose(
                &config,
                &provider,
                &runtimes,
                "s",
                1.0,
                Some(Arc::new(move |message: String, _: Option<RuntimeError>| {
                    sink.lock().expect("#1433 diagnostics lock").push(message);
                })),
            )
            .await,
            "#1433 composition succeeds",
        );
        crate::negative_assert!(
            tools
                .shared()
                .iter()
                .any(|tool| wire_name(tool) == "mcp__shared-echo__echo"),
            "#1433 the limit keeps the shared tools"
        );
        crate::negative_assert!(
            tools.per_session().is_empty(),
            "#1433 the limit skips the per-session server"
        );
        crate::negative_assert_eq!(
            runtimes.size(),
            1,
            "#1433 the limit registers no per-session runtime"
        );
        crate::negative_assert!(
            diagnostics
                .lock()
                .expect("#1433 diagnostics lock")
                .iter()
                .any(|message| message.starts_with(
                    "Per-session MCP limit reached (1). Session s will not start: private-echo."
                )),
            "#1433 the limit reports the TS diagnostic"
        );
        release.release().await.expect("#1433 composition release");
    }

    /// `compose` (#2442): the cap comparison and the registration share one
    /// critical section (`register_below_cap`). Two concurrent prepares on
    /// the same factory-level registry — both parked inside the runtime
    /// factory, exactly the interleave where the old two-lock
    /// check-then-register had both already passed the size check — register
    /// at most one runtime under cap=1; the other reports the TS diagnostic.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn session_mcp_tools_holds_the_cap_across_concurrent_prepares() {
        let config = test_config();
        for session_key in ["s-a", "s-b"] {
            config
                .set_mcp_servers(&set_input(session_key, &[("private-echo", true)]))
                .await
                .expect("#1433 fixture servers commit");
        }
        // Both prepares park inside the factory until the second arrives, so
        // neither's registration can precede the other's cap check.
        let parked = Arc::new(std::sync::Barrier::new(2));
        let provider = Arc::new(ProjectMcpRuntimeProvider::new(
            ProjectMcpRuntimeProviderOptions {
                project_root: "/pdrs-fixture/project".into(),
                pilot_home: "/pdrs-fixture/home".into(),
                create_runtime: Some(Arc::new(move |_: Vec<McpServerSpec>| {
                    parked.wait();
                    let runtime: Arc<dyn McpRuntimePort> = Arc::new(FakeMcpRuntime {
                        label: "per-session",
                        tools: Vec::new(),
                        start_fails: false,
                        events: Arc::new(Mutex::new(Vec::new())),
                    });
                    runtime
                })),
                proxy: None,
                on_diagnostic: None,
                environment: Arc::new(SystemEnvironment),
                builtin_paths: BuiltinMcpPaths::default(),
            },
        ));
        let runtimes = SessionMcpRuntimeRegistry::new();
        let diagnostics: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let prepare = |session_key: &'static str| {
            let config = config.clone();
            let provider = provider.clone();
            let runtimes = runtimes.clone();
            let diagnostics = diagnostics.clone();
            tokio::spawn(async move {
                let sink = Arc::new(move |message: String, _: Option<RuntimeError>| {
                    diagnostics
                        .lock()
                        .expect("#2442 diagnostics lock")
                        .push(message);
                });
                crate::test_negative_receipt::expect_ok(
                    compose(&config, &provider, &runtimes, session_key, 1.0, Some(sink)).await,
                    "#2442 the concurrent composition succeeds",
                )
            })
        };
        let a = prepare("s-a");
        let b = prepare("s-b");
        let (_tools_a, release_a) = a.await.expect("#2442 prepare s-a joins");
        let (_tools_b, release_b) = b.await.expect("#2442 prepare s-b joins");
        crate::negative_assert_eq!(
            runtimes.size(),
            1,
            "#2442 concurrent prepares register at most one runtime under the cap"
        );
        let limited: Vec<String> = diagnostics.lock().expect("#2442 diagnostics lock").clone();
        crate::negative_assert_eq!(
            limited.len(),
            1,
            "#2442 exactly one concurrent prepare reports the limit diagnostic"
        );
        crate::negative_assert!(
            limited[0]
                == "Per-session MCP limit reached (1). Session s-a will not start: private-echo."
                || limited[0]
                    == "Per-session MCP limit reached (1). Session s-b will not start: private-echo.",
            "#2442 the limit diagnostic names one of the concurrent sessions"
        );
        release_a.release().await.expect("#2442 release s-a");
        release_b.release().await.expect("#2442 release s-b");
        crate::negative_assert_eq!(
            runtimes.size(),
            0,
            "#2442 the registered runtime is disposed on release"
        );
    }

    /// `compose` (#1433 fix-r1 review P2#4): the limit is the snapshot's f64,
    /// compared without truncation. The TS check is
    /// `perSessionRuntimes.size >= maxPerSessionInstances`
    /// (SessionMcpRuntimeBundle.ts:70), where a cap of 2.5 still admits the
    /// third registration (`2 >= 2.5` is false in Node v22.22.0; the
    /// `as usize` cast used to floor 2.5 to 2 and refuse it).
    #[tokio::test]
    async fn session_mcp_tools_allows_the_third_runtime_under_a_fractional_limit() {
        let config = test_config();
        config
            .set_mcp_servers(&set_input(
                "s",
                &[("shared-echo", false), ("private-echo", true)],
            ))
            .await
            .expect("#1433 fixture servers commit");
        let provider = fake_provider(Arc::new(Mutex::new(Vec::new())), false);
        let runtimes = SessionMcpRuntimeRegistry::new();
        for key in ["other-a", "other-b"] {
            runtimes
                .register(
                    key,
                    Arc::new(FakeMcpRuntime {
                        label: "per-session",
                        tools: Vec::new(),
                        start_fails: false,
                        events: Arc::new(Mutex::new(Vec::new())),
                    }),
                )
                .expect("#1433 pre-existing per-session runtime");
        }
        let (tools, release) = crate::test_negative_receipt::expect_ok(
            compose(&config, &provider, &runtimes, "s", 2.5, None).await,
            "#1433 composition succeeds under a fractional cap",
        );
        crate::negative_assert!(
            tools
                .per_session()
                .iter()
                .any(|tool| wire_name(tool) == "mcp__private-echo__echo"),
            "#1433 a 2.5 cap still admits the third per-session runtime"
        );
        crate::negative_assert_eq!(
            runtimes.size(),
            3,
            "#1433 the fractional cap registers the third runtime"
        );
        // At size 3 the cap binds again (`3 >= 2.5`), reported with the f64.
        let diagnostics: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let sink = diagnostics.clone();
        let (tools, limited_release) = crate::test_negative_receipt::expect_ok(
            compose(
                &config,
                &provider,
                &runtimes,
                "s",
                2.5,
                Some(Arc::new(move |message: String, _: Option<RuntimeError>| {
                    sink.lock().expect("#1433 diagnostics lock").push(message);
                })),
            )
            .await,
            "#1433 composition succeeds at the fractional cap",
        );
        crate::negative_assert!(
            tools.per_session().is_empty(),
            "#1433 the fractional cap skips the per-session server at size 3"
        );
        crate::negative_assert!(
            diagnostics
                .lock()
                .expect("#1433 diagnostics lock")
                .iter()
                .any(|message| message.starts_with(
                    "Per-session MCP limit reached (2.5). Session s will not start: private-echo."
                )),
            "#1433 the fractional cap reports the TS diagnostic with the f64 limit"
        );
        release.release().await.expect("#1433 composition release");
        limited_release
            .release()
            .await
            .expect("#1433 composition release");
        crate::negative_assert_eq!(
            runtimes.size(),
            2,
            "#1433 both releases dispose their own obligations"
        );
    }

    /// `compose` (#1433): a per-session start failure degrades to a
    /// diagnostic; the registration is still released at close, never leaked.
    #[tokio::test]
    async fn session_mcp_tools_degrades_per_session_start_failures() {
        let config = test_config();
        config
            .set_mcp_servers(&set_input("s", &[("private-echo", true)]))
            .await
            .expect("#1433 fixture servers commit");
        let provider = fake_provider(Arc::new(Mutex::new(Vec::new())), true);
        let runtimes = SessionMcpRuntimeRegistry::new();
        let diagnostics: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let sink = diagnostics.clone();
        let (tools, release) = crate::test_negative_receipt::expect_ok(
            compose(
                &config,
                &provider,
                &runtimes,
                "s",
                5.0,
                Some(Arc::new(move |message: String, _: Option<RuntimeError>| {
                    sink.lock().expect("#1433 diagnostics lock").push(message);
                })),
            )
            .await,
            "#1433 composition survives the start failure",
        );
        crate::negative_assert!(
            tools.is_empty(),
            "#1433 a failed per-session start composes no tools"
        );
        crate::negative_assert_eq!(
            runtimes.size(),
            1,
            "#1433 the failed start keeps its registration for close"
        );
        crate::negative_assert!(
            diagnostics
                .lock()
                .expect("#1433 diagnostics lock")
                .iter()
                .any(|message| message == "Per-session MCP startup failed for s"),
            "#1433 the start failure reports the TS diagnostic"
        );
        release.release().await.expect("#1433 composition release");
        crate::negative_assert_eq!(
            runtimes.size(),
            0,
            "#1433 the failed start is disposed on release"
        );
    }

    /// `SessionTools::turn` (#1433): the composed MCP tools reach the turn's
    /// tool registry through `native_tools`, next to the builtins and LSP,
    /// under the same allowed/denied policy.
    #[tokio::test]
    async fn native_turn_registry_carries_the_session_mcp_tools() {
        let config = test_config();
        config
            .set_mcp_servers(&set_input("s", &[("a", true)]))
            .await
            .expect("#1433 fixture servers commit");
        let provider = fake_provider(Arc::new(Mutex::new(Vec::new())), false);
        let (tools, release) = crate::test_negative_receipt::expect_ok(
            compose(
                &config,
                &provider,
                &SessionMcpRuntimeRegistry::new(),
                "s",
                5.0,
                None,
            )
            .await,
            "#1433 composition succeeds",
        );
        let name: JsString = "mcp__a__echo".into();
        let registry = native_tools(
            None,
            None,
            Arc::new(super::super::families::DefaultLsp::default()),
            &tools,
        );
        crate::negative_assert!(
            registry.has(&name),
            "#1433 the turn registry carries the session MCP tool"
        );
        crate::negative_assert!(
            registry.has(&"read_file".into()),
            "#1433 the turn registry keeps the read-only builtins"
        );
        let restricted = native_tools(
            Some(vec![name.clone()]),
            None,
            Arc::new(super::super::families::DefaultLsp::default()),
            &tools,
        );
        let listed = restricted
            .list()
            .expect("#1433 restricted tool catalog")
            .len();
        crate::negative_assert_eq!(
            listed,
            1,
            "#1433 the allowlist admits only the session MCP tool"
        );
        crate::negative_assert!(
            restricted.has(&name),
            "#1433 the allowlist admits the session MCP tool"
        );
        let denied = native_tools(
            None,
            Some(vec![name]),
            Arc::new(super::super::families::DefaultLsp::default()),
            &tools,
        );
        crate::negative_assert!(
            !denied.has(&"mcp__a__echo".into()),
            "#1433 the denylist excludes the session MCP tool"
        );
        crate::negative_assert!(
            denied.has(&"read_file".into()),
            "#1433 the denylist keeps the unrelated builtins"
        );
        release.release().await.expect("#1433 composition release");
    }

    fn bridge_tool(wire_name: &str) -> Arc<McpBridgeTool> {
        Arc::new(McpBridgeTool::new(
            McpToolSpec {
                server_id: "a".into(),
                tool_name: "t".into(),
                wire_name: wire_name.into(),
                description: "fixture".into(),
                input_schema: pilotdeck_protocol::parse_json("{}").expect("#1433 fake tool schema"),
                annotations: None,
                meta: None,
            },
            Arc::new(FakeMcpRuntime {
                label: "per-session",
                tools: Vec::new(),
                start_fails: false,
                events: Arc::new(Mutex::new(Vec::new())),
            }),
            CreateToolDefinitionsOptions::default(),
        ))
    }

    /// The registration tail (#1433): shared-lease tools keep existing names,
    /// per-session tools replace them (SessionMcpRuntimeBundle.ts:61-64,
    /// 138-144). `kind: "mcp"` marks a bridge tool; read_file carries
    /// `kind: "filesystem"`, so the field separates the two even though both
    /// definitions also set `maxResultBytes`.
    #[test]
    fn register_keeps_shared_names_and_replaces_per_session_names() {
        let base = native_tools(
            None,
            None,
            Arc::new(super::super::families::DefaultLsp::default()),
            &SessionMcpTools::empty(),
        );
        let is_mcp = |name: &str| {
            base.get(&name.into())
                .expect("#1433 fixture tool present")
                .fields()
                .get("kind")
                .and_then(pilotdeck_protocol::JsValue::as_string)
                .is_some_and(|kind| kind.equals_str("mcp"))
        };
        crate::negative_assert!(
            !is_mcp("read_file"),
            "#1433 the fixture starts on the builtin read tool"
        );
        SessionMcpTools {
            shared: vec![bridge_tool("read_file")],
            per_session: Vec::new(),
        }
        .register(&base);
        crate::negative_assert!(
            !is_mcp("read_file"),
            "#1433 a shared session MCP tool keeps the existing builtin name"
        );
        SessionMcpTools {
            shared: Vec::new(),
            per_session: vec![bridge_tool("read_file")],
        }
        .register(&base);
        crate::negative_assert!(
            is_mcp("read_file"),
            "#1433 a per-session MCP tool replaces the same-name builtin"
        );
    }
}
