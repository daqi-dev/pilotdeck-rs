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
    sync::{Arc, Mutex},
    time::Duration,
};

pub struct SessionTools {
    options: Options,
    env: JsObject,
    binding: SessionBinding,
    config: JsObject,
    interactions: Arc<GatewayInteractionCoordinator>,
    permission: Arc<Permissions>,
    lsp: Arc<super::families::DefaultLsp>,
}
impl SessionTools {
    pub fn new(
        options: &Options,
        env: &crate::config::Env,
        binding: SessionBinding,
        config: JsObject,
        interactions: Arc<GatewayInteractionCoordinator>,
        lsp: Arc<super::families::DefaultLsp>,
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
            permission: Arc::new(Permissions(Mutex::new((context, None)))),
        })
    }
    pub fn turn(
        &self,
        identity: &ExecutionIdentity,
        recorder: Arc<Recorder>,
        signal: Cancellation,
    ) -> SessionPorts {
        let tools = native_tools(
            self.binding.context.allowed_tools.clone(),
            self.binding.context.disallowed_tools.clone(),
            self.lsp.clone(),
        );
        let execution = Arc::new(ToolRuntime::new(
            tools,
            self.permission.clone(),
            SchedulerMode::Concurrent,
        ));
        let mut config = self.config.clone();
        config.insert("env".into(), JsValue::Object(self.env.clone()));
        config.insert(
            "permissionContext".into(),
            self.permission
                .0
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .0
                .to_value(),
        );
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
            tool_results_dir: self
                .options
                .project
                .join(".pilotdeck/tool-results")
                .join(crate::storage::sanitize_session_id(&identity.session_id)),
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
    tools
}
