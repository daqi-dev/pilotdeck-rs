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
    user_dialog: Option<super::assembly::user_dialog::NativeDialogConfig>,
    automation: Option<super::session_overrides::AutomationOverrides>,
    pinned_steer: Option<crate::runtime::project::interaction_steer::InteractionSteer>,
    session_emit: Option<super::session_events::SessionEmit>,
    forks: Option<Arc<super::agent::NativeForkRegistry>>,
    fork_owner: Option<Arc<super::agent::NativeForkOwner>>,
    fork_scope: Option<Arc<super::agent::ForkScope>>,
    web: Option<super::web::NativeWebTools>,
    skills: super::skills::NativeSkills,
    knowledge: Option<Arc<super::knowledge::KnowledgeTool>>,
    context_runtime: Option<Arc<dyn crate::host::modules::context::ContextRuntime>>,
    options: Options,
    env: JsObject,
    binding: SessionBinding,
    config: JsObject,
    interactions: Arc<GatewayInteractionCoordinator>,
    permission: Arc<Permissions>,
    permission_rules: crate::config::permission::RuleSetHandle,
    lsp: Arc<super::families::DefaultLsp>,
    #[cfg(unix)]
    background_tasks: Option<crate::runtime::background::BackgroundTaskRuntime>,
    #[cfg(unix)]
    native_sandbox: Option<Arc<crate::runtime::tools::sandbox::NativeSandbox>>,
    /// #1433: the session's composed MCP tools, registered into every turn's
    /// tool registry after the LSP entry (composed once at prepare, like
    /// `SessionMcpRuntimeBundle.compose`).
    mcp: SessionMcpTools,
    /// #1864 (b): re-read on every turn, like InProcessGateway.ts:1443 —
    /// the host-side permission context follows the stored selection so the
    /// same turn never runs one mode for the sidecar and another for native
    /// tool admission.
    permission_mode_source: Option<Arc<dyn Fn() -> Option<JsString> + Send + Sync>>,
    goal: crate::runtime::goal::GoalRuntime,
    plugin_hooks: crate::hooks::protocol::HooksSettings,
    hook_proxy: Option<crate::runtime::model::http::ProxySettings>,
    hook_env: crate::config::Env,
    plan_files: Option<Arc<super::plan_todo::NativePlanFiles>>,
    plan_serial: Arc<tokio::sync::Mutex<()>>,
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
                cwd: config
                    .get("cwd")
                    .and_then(JsValue::as_string)
                    .cloned()
                    .unwrap_or_else(|| options.project.to_string_lossy().into_owned().into()),
                additional_working_directories: Some(super::sdk_permission::directories(&config)),
                can_prompt: Some(false),
                bypass_available: Some(
                    config.get("bypassAvailable") != Some(&JsValue::Bool(false)),
                ),
                ..Default::default()
            });
        let goal = crate::runtime::goal::GoalRuntime::new(
            binding.context.session_key.clone(),
            binding.recorder.clone(),
            binding.projection.clone(),
        );
        Ok(Self {
            user_dialog: None,
            automation: None,
            pinned_steer: None,
            session_emit: None,
            forks: None,
            fork_owner: None,
            fork_scope: None,
            goal,
            plugin_hooks: Vec::new(),
            hook_proxy: None,
            hook_env: env.clone(),
            plan_files: None,
            plan_serial: Arc::new(tokio::sync::Mutex::new(())),
            web: None,
            skills: Default::default(),
            knowledge: None,
            context_runtime: None,
            options: options.clone(),
            env: {
                let mut env = env_object(env);
                env.insert(
                    "PILOT_HOME".into(),
                    JsValue::String(options.home.to_string_lossy().into_owned().into()),
                );
                env
            },
            binding,
            config,
            interactions,
            lsp,
            mcp,
            permission: Arc::new(Permissions(Mutex::new((context, None)))),
            permission_rules: crate::config::permission::RuleSetHandle::empty(),
            permission_mode_source: None,
            #[cfg(unix)]
            background_tasks: None,
            #[cfg(unix)]
            native_sandbox: None,
        })
    }

    pub(super) fn with_knowledge(
        mut self,
        tool: Option<Arc<super::knowledge::KnowledgeTool>>,
    ) -> Self {
        self.knowledge = tool;
        self
    }

    pub(super) fn with_user_dialog(
        mut self,
        config: Option<super::assembly::user_dialog::NativeDialogConfig>,
    ) -> Self {
        self.user_dialog = config;
        self
    }

    fn execution_cwd(&self) -> JsString {
        self.config
            .get("cwd")
            .and_then(JsValue::as_string)
            .cloned()
            .unwrap_or_else(|| self.options.project.to_string_lossy().as_ref().into())
    }

    pub(super) fn with_automation(
        mut self,
        automation: Option<super::session_overrides::AutomationOverrides>,
    ) -> Self {
        if let Some(automation) = &automation {
            let denied = self
                .binding
                .context
                .disallowed_tools
                .get_or_insert_with(Vec::new);
            for name in &automation.exclude_tools {
                if !denied.contains(name) {
                    denied.push(name.clone());
                }
            }
        }
        self.automation = automation;
        self
    }

    pub(super) fn with_plan_files(mut self, files: Arc<super::plan_todo::NativePlanFiles>) -> Self {
        self.plan_files = Some(files);
        self
    }

    pub(super) fn with_pinned_steer(
        mut self,
        steer: Option<crate::runtime::project::interaction_steer::InteractionSteer>,
    ) -> Self {
        self.pinned_steer = steer;
        self
    }

    pub(super) fn with_session_emitter(mut self, emit: super::session_events::SessionEmit) -> Self {
        self.session_emit = Some(emit);
        self
    }

    pub(super) fn with_forks(
        mut self,
        forks: Arc<super::agent::NativeForkRegistry>,
        scope: Option<Arc<super::agent::ForkScope>>,
        owner: Arc<super::agent::NativeForkOwner>,
    ) -> Self {
        self.forks = Some(forks);
        self.fork_scope = scope;
        self.fork_owner = Some(owner);
        self
    }
    pub(super) fn with_fork_mcp_names(mut self, names: Vec<JsString>) -> Self {
        if let Some(allowed) = &mut self.binding.context.allowed_tools {
            for name in names {
                if !allowed.contains(&name) {
                    allowed.push(name);
                }
            }
        }
        self
    }
    pub(super) fn with_plugin_hooks(
        mut self,
        hooks: crate::hooks::protocol::HooksSettings,
    ) -> Self {
        self.plugin_hooks = hooks;
        self
    }
    pub(super) fn with_hook_proxy(
        mut self,
        proxy: Option<crate::runtime::model::http::ProxySettings>,
    ) -> Self {
        self.hook_proxy = proxy;
        self
    }

    pub(super) fn with_skills(
        mut self,
        skills: super::skills::NativeSkills,
        context: Arc<dyn crate::host::modules::context::ContextRuntime>,
    ) -> Self {
        self.skills = skills;
        self.context_runtime = Some(context);
        self
    }

    pub(super) fn with_web(mut self, web: super::web::NativeWebTools) -> Self {
        self.web = Some(web);
        self
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

    pub fn with_permission_rules(
        mut self,
        rules: crate::config::permission::RuleSetHandle,
    ) -> Self {
        self.permission_rules = rules;
        self
    }

    #[cfg(unix)]
    pub fn with_background_tasks(
        mut self,
        tasks: crate::runtime::background::BackgroundTaskRuntime,
    ) -> Self {
        self.background_tasks = Some(tasks);
        self
    }

    #[cfg(unix)]
    pub fn with_native_sandbox(
        mut self,
        sandbox: Arc<crate::runtime::tools::sandbox::NativeSandbox>,
    ) -> Self {
        self.native_sandbox = Some(sandbox);
        self
    }

    pub(super) fn tool_results_dir(&self, session: &JsString) -> PathBuf {
        let root = self
            .config
            .get("nativeEphemeralRoot")
            .and_then(JsValue::as_string)
            .and_then(|v| v.to_utf8().ok())
            .map(PathBuf::from)
            .unwrap_or_else(|| self.options.project.clone());
        session_tool_results_dir(&root, session)
    }

    #[cfg(test)]
    pub fn turn(
        &self,
        identity: &ExecutionIdentity,
        recorder: Arc<Recorder>,
        signal: Cancellation,
    ) -> SessionPorts {
        self.turn_with_authority(identity, recorder, signal, None)
    }

    #[cfg(test)]
    pub fn turn_with_authority(
        &self,
        identity: &ExecutionIdentity,
        recorder: Arc<Recorder>,
        signal: Cancellation,
        authority: Option<crate::runtime::turn::interaction::TurnInteractionAuthority>,
    ) -> SessionPorts {
        let settings = self
            .persisted_permissions_home()
            .map(|home| super::persisted_permissions::read(&home))
            .unwrap_or_default();
        self.turn_with_authority_and_settings(identity, recorder, signal, authority, settings)
    }

    pub(super) fn persisted_permissions_home(&self) -> Option<PathBuf> {
        self.fork_scope.is_none().then(|| self.options.home.clone())
    }

    pub(super) fn turn_with_authority_and_settings(
        &self,
        identity: &ExecutionIdentity,
        recorder: Arc<Recorder>,
        signal: Cancellation,
        authority: Option<crate::runtime::turn::interaction::TurnInteractionAuthority>,
        persisted: super::persisted_permissions::Settings,
    ) -> SessionPorts {
        let persisted_skip = persisted.skip_permissions;
        let persisted_error = persisted.error;
        let turn_rules = if self.fork_scope.is_some() {
            self.permission_rules.clone()
        } else {
            self.permission_rules.with_user_rules(persisted.rules)
        };
        // #1864 (b): the effective mode — a non-empty
        // `managedPermissions.defaultMode` wins, else the stored selection —
        // is written back before the context is emitted, so `Permissions::
        // decide` (which clones the current value per call), the emitted
        // `permissionContext` and this turn's local `config.permissionMode`
        // (`tool_context` and `resolve_workspace_path` read that copy) all
        // agree with the sidecar's payload this turn. A `None` source leaves
        // the previous mode and the plan-round-trip stack below untouched.
        let (mut turn_context, permission_mode) = {
            let mut state = self.permission.0.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(mode) = permission::configured_permission_mode(&self.config)
                .or_else(|| {
                    self.permission_mode_source
                        .as_ref()
                        .and_then(|source| source())
                })
                .or_else(|| {
                    // Host default applies only below managed and stored selections.
                    // Re-reading false resets a previous persisted bypass on next turn.
                    (self.permission_mode_source.is_some() && self.fork_scope.is_none()).then(
                        || {
                            if persisted_skip {
                                "bypassPermissions".into()
                            } else {
                                "default".into()
                            }
                        },
                    )
                })
            {
                state.0.mode = crate::config::permission::PermissionMode::from(mode);
            }
            // TS HostPermissionModeState.applyTurnInput runs after session config
            // composition. Mode comes from validated host admission, not a tool
            // payload; policy rules still run before bypass in permission::decide.
            if let Some(mode) = authority.as_ref().and_then(|a| a.permission_mode.as_ref()) {
                if mode.equals_str("plan") && !state.0.mode.as_js_string().equals_str("plan") {
                    state.1 = Some(
                        authority
                            .as_ref()
                            .and_then(|a| a.base_permission_mode.clone())
                            .map(crate::config::permission::PermissionMode::from)
                            .unwrap_or_else(|| state.0.mode.clone()),
                    );
                }
                state.0.mode = crate::config::permission::PermissionMode::from(mode.clone());
            }
            (state.0.clone(), state.0.mode.as_js_string().clone())
        };
        // These flags belong to this invocation, never the session's shared
        // permission-mode state. A managed false remains a policy ceiling.
        let policy_disabled = self.config.get("canPrompt") == Some(&JsValue::Bool(false))
            || self
                .config
                .get("managedPermissions")
                .and_then(|v| v.get("canPrompt"))
                == Some(&JsValue::Bool(false))
            || authority
                .as_ref()
                .is_some_and(|a| a.policy_can_prompt == Some(false));
        let sdk_mode = authority
            .as_ref()
            .and_then(|a| a.sdk_permission_mode.as_ref());
        turn_context.accept_edits = sdk_mode.map(|m| m.equals_str("acceptEdits"));
        turn_context.can_prompt = authority
            .as_ref()
            .is_some_and(|a| a.can_prompt.unwrap_or(true))
            && !policy_disabled
            && !sdk_mode.is_some_and(|m| m.equals_str("dontAsk"));
        turn_context.policy_can_prompt = policy_disabled.then_some(false);
        if let Some(automation) = &self.automation {
            turn_context.can_prompt &= automation.can_prompt;
            if !automation.can_prompt {
                turn_context.policy_can_prompt = Some(false);
            }
            turn_context.bypass_available &= automation.bypass_available;
        }
        turn_context.rules = turn_rules.snapshot();
        if let Some(files) = &self.plan_files {
            turn_context.plan_directory_path = Some(files.directory_string());
        }
        let permission_context = turn_context.to_value();
        let permission_emit = authority
            .as_ref()
            .map(|a| a.emit.clone())
            .unwrap_or_else(|| Arc::new(|_| false));
        let execute_code_web_search = self.web.as_ref().is_some_and(|web| web.search_enabled());
        #[cfg(unix)]
        let tools = if let Some(sandbox) = &self.native_sandbox {
            native_tools_with_sandbox(
                execute_code_web_search,
                self.binding.context.allowed_tools.clone(),
                self.binding.context.disallowed_tools.clone(),
                self.lsp.clone(),
                sandbox.clone(),
                &self.mcp,
            )
        } else {
            native_tools_profile(
                execute_code_web_search,
                self.binding.context.allowed_tools.clone(),
                self.binding.context.disallowed_tools.clone(),
                self.lsp.clone(),
                &self.mcp,
            )
        };
        #[cfg(not(unix))]
        let tools = native_tools_profile(
            execute_code_web_search,
            self.binding.context.allowed_tools.clone(),
            self.binding.context.disallowed_tools.clone(),
            self.lsp.clone(),
            &self.mcp,
        );
        let mut read_env = self.env.clone();
        read_env.insert(
            "PILOT_HOME".into(),
            JsValue::String(self.options.home.to_string_lossy().into_owned().into()),
        );
        tools
            .replace(Arc::new(
                builtin::read_file::ReadFileTool::with_permissions(
                    self.binding.checkpoint.clone(),
                    read_env,
                ),
            ))
            .expect("native read_file is registered");
        if self.user_dialog.is_some()
            && let Some(JsValue::Array(kinds)) = self.config.get("userDialogKinds")
        {
            let kinds: Vec<_> = kinds
                .iter()
                .filter_map(JsValue::as_string)
                .cloned()
                .collect();
            builtin::user_dialog::register_user_dialog_tools(&tools, &kinds)
                .expect("native opt-in dialog tool names are unique");
        }
        tools
            .register(Arc::new(builtin::send_attachment::SendAttachmentTool::new(
                self.binding.checkpoint.clone(),
                self.env.clone(),
            )))
            .expect("native attachment tool name is unique");
        if self.plan_files.is_some() {
            for kind in [
                builtin::plan_todo::Kind::Enter,
                builtin::plan_todo::Kind::Exit,
                builtin::plan_todo::Kind::Todo,
            ] {
                tools
                    .register(Arc::new(builtin::plan_todo::Tool::new(kind)))
                    .expect("native plan tool names are unique");
            }
        }
        if let Some(contexts) = self
            .automation
            .as_ref()
            .and_then(|value| value.context_registry.as_ref())
        {
            crate::runtime::always_on::register_tools(
                &tools,
                contexts.clone(),
                &identity.session_id,
            )
            .expect("native automation tool names are unique");
        }
        self.skills.register(&tools);
        let knowledge_catalog_error: Option<JsString> = self
            .knowledge
            .as_ref()
            .and_then(|knowledge| tools.register(knowledge.clone()).err())
            .map(|error| error.message.into());
        if let Some(web) = &self.web {
            web.register(&tools, identity)
                .expect("native web tool names are unique");
        }
        #[cfg(unix)]
        if let Some(tasks) = &self.background_tasks {
            for tool in if let Some(sandbox) = &self.native_sandbox {
                builtin::task::task_tools_with_sandbox(Some(tasks.clone()), sandbox.clone())
            } else {
                builtin::task::task_tools(Some(tasks.clone()))
            } {
                tools.register(tool).expect("native task names are unique");
            }
        }
        let mut config = self.config.clone();
        config.insert("env".into(), JsValue::Object(self.env.clone()));
        config.insert("permissionMode".into(), JsValue::String(permission_mode));
        config.insert("permissionContext".into(), permission_context);
        // The SDK can only narrow the complete ordinary catalog. Capture this
        // same view in forks and native execution so refresh/aliases cannot
        // reintroduce a blocked native, extension or MCP definition.
        let tools = super::sdk_tool_policy::scope(
            tools,
            &self.config,
            self.binding.context.allowed_tools.clone(),
            self.binding.context.disallowed_tools.clone(),
        )
        .expect("new native turn registry is active");
        let fork_turn = self.forks.as_ref().map(|registry| {
            super::agent::ForkTurn::new(
                registry.clone(),
                identity.clone(),
                config.clone(),
                recorder.clone(),
                turn_rules.clone(),
                authority.clone(),
                self.fork_scope.as_deref(),
                tools.clone(),
                self.skills.clone(),
                self.fork_owner.as_ref(),
                #[cfg(unix)]
                self.native_sandbox.clone(),
            )
        });
        if let Some(turn) = &fork_turn {
            if let Some(root) = &self.binding.history_root {
                turn.bind_history_root(root.clone());
            }
            if let Some(emit) = &self.session_emit {
                turn.bind_session_emitter(emit.clone());
            }
            turn.register(&tools);
        }
        let sdk_catalog_error = super::sdk_tools::install(&tools, &config)
            .err()
            .map(|error| error.message.into());
        let execution = ToolRuntime::new(
            tools,
            // Even a profile without an interaction resource keeps this turn's
            // policy/flags/rules; session mode observation is a separate port.
            Arc::new(Permissions(Mutex::new((turn_context.clone(), None)))),
            SchedulerMode::Concurrent,
        )
        .with_catalog_error(
            persisted_error
                .or(knowledge_catalog_error)
                .or(sdk_catalog_error),
        )
        .with_event_emitter({
            let emit = permission_emit.clone();
            Arc::new(move |event| {
                // Match GatewayAgentEventProjector's PermissionDenied projection:
                // internal sessionId/turnId stay behind the Gateway wire boundary.
                let mut frame = JsObject::new();
                for key in ["type", "toolName", "reason"] {
                    frame.insert(
                        key.into(),
                        event.get(key).cloned().unwrap_or(JsValue::Undefined),
                    );
                }
                let _ = emit(JsValue::Object(frame));
            })
        })
        .with_native_sandbox(
            self.config
                .get("sandboxMode")
                .and_then(JsValue::as_string)
                .cloned(),
        );
        #[cfg(unix)]
        let execution = if let Some(sandbox) = &self.native_sandbox {
            execution.with_native_sandbox_provider(sandbox.clone())
        } else {
            execution
        };
        let execution = Arc::new(execution.with_mode_owner(self.permission.clone()));
        let execution: Arc<dyn capability::ToolExecution> = if let Some(turn) = &fork_turn {
            Arc::new(super::agent::ForkExecution::new(execution, turn.clone()))
        } else {
            execution
        };
        let binding = TurnBinding {
            config: config.clone(),
            input: value([
                ("sessionId", JsValue::String(identity.session_id.clone())),
                ("turnId", JsValue::String(identity.turn_id.clone())),
                ("canElicit", JsValue::Bool(self.fork_scope.is_none())),
            ]),
            checkpoint: self.binding.checkpoint.clone(),
            abort_signal: Some(Arc::new(signal.clone())),
            environment: Arc::new(NativeTurnEnvironment(self.env.clone())),
        };
        let question_profile = Arc::new(std::sync::OnceLock::new());
        let mut sdk_state = authority
            .as_ref()
            .and_then(|authority| authority.sdk_hooks.clone());
        let on_requested = sdk_state.as_ref().map(|sdk| {
            let handlers = sdk.handlers.clone();
            let session = identity.session_id.clone();
            Arc::new(move |payload: JsObject| {
                let handler = handlers
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .session_hook_handler(&session)
                    .cloned();
                if let Some(handler) = handler {
                    handler(
                        crate::runtime::sdk_session::PilotDeckHookEvent::Elicitation,
                        &JsValue::Object(payload),
                    );
                }
            }) as super::elicitation::RequestedObserver
        });
        let elicitation = super::elicitation::Elicitation {
            profile: question_profile.clone(),
            coordinator: self.interactions.clone(),
            on_requested,
            session: identity.session_id.clone(),
            can_prompt: turn_context.can_prompt,
            emit: permission_emit.clone(),
            timeout: Duration::from_millis(
                self.env
                    .get("PILOTDECK_ELICITATION_TIMEOUT_MS")
                    .and_then(JsValue::as_string)
                    .and_then(|s| s.to_utf8().ok())
                    .map(|s| s.trim().to_owned())
                    .and_then(|s| {
                        let s = s.strip_prefix('+').unwrap_or(&s);
                        let digits: String = s.chars().take_while(char::is_ascii_digit).collect();
                        digits.parse::<u64>().ok().filter(|n| *n > 0)
                    })
                    .unwrap_or(120_000),
            ),
        };
        let profile_for_bind = question_profile.clone();
        let profile_for_presence = question_profile;
        let mode_owner = self.permission.clone();
        let plan_todo = self.plan_files.as_ref().map(|_| {
            Arc::new(super::plan_todo::PlanTodo {
                session: identity.session_id.clone(),
                turn: identity.turn_id.clone(),
                projection: self.binding.projection.clone(),
                recorder: self.binding.recorder.clone(),
                serial: self.plan_serial.clone(),
            })
        });
        let sdk_file_slot = sdk_state
            .as_ref()
            .filter(|sdk| {
                sdk.settings.iter().any(|(event, matchers)| {
                    event.equals_str("FileChanged") && !matchers.is_empty()
                })
            })
            .map(|_| {
                Arc::new(std::sync::OnceLock::<(
                    Arc<crate::runtime::hooks::LifecycleRuntime>,
                    Cancellation,
                )>::new())
            });
        let sdk_file_notifier = sdk_file_slot.as_ref().map(|slot| {
            let slot = slot.clone();
            let session = identity.session_id.clone();
            let cwd = self.execution_cwd();
            let callback: Arc<
                dyn Fn(
                        crate::runtime::tools::builtin::FileUpdateNotice,
                    ) -> crate::host::HostFuture<()>
                    + Send
                    + Sync,
            > = Arc::new(move |notice| {
                let slot = slot.clone();
                let session = session.clone();
                let cwd = cwd.clone();
                Box::pin(async move {
                    let (runtime, signal) = slot.get().ok_or_else(|| {
                        crate::host::ModuleError::new("SDK FileChanged owner is unavailable.")
                    })?;
                    let input = crate::runtime::hooks::LifecycleDispatchInput {
                        event: "FileChanged".into(),
                        base_input: value([
                            ("sessionId", JsValue::String(session)),
                            ("transcriptPath", "".into()),
                            ("cwd", JsValue::String(cwd.clone())),
                        ]),
                        payload: Some(value([
                            ("filePath", JsValue::String(notice.relative.clone())),
                            ("absolutePath", JsValue::String(notice.absolute)),
                            ("root", JsValue::String(notice.root)),
                            (
                                "changeType",
                                if notice.created {
                                    "created".into()
                                } else {
                                    "updated".into()
                                },
                            ),
                        ])),
                        match_query: Some(notice.relative),
                        cwd,
                        env: None,
                        signal: Some(signal.clone()),
                    };
                    let _ = runtime.dispatch(input).await;
                    Ok(())
                })
            });
            Arc::new(crate::runtime::tools::builtin::FileUpdateNotifier(callback))
                as capability::RuntimeContext
        });
        let user_dialog = self.user_dialog.as_ref().map(|dialog| {
            let emit = permission_emit.clone();
            let mut options = crate::user_dialog::ChannelOptions::new(
                identity.session_id.clone(),
                dialog.bus.clone(),
                Arc::new(move |event| {
                    if emit(event) {
                        Ok(())
                    } else {
                        Err(crate::user_dialog::DialogError::Invalid(
                            "User dialog event owner is no longer active.".into(),
                        ))
                    }
                }),
            );
            options.journal = dialog.journal.clone();
            Arc::new(dialog.port.with_options(options)) as capability::RuntimeContext
        });
        let tool_ports = ToolContextPorts {
            file_history: self
                .binding
                .file_history
                .clone()
                .map(|history| history as crate::host::modules::capability::RuntimeContext),
            // ToolRuntimeContext creates this callback only when the frozen
            // session config enables includeToolProgress.  Project exactly
            // the gateway tool_progress shape before the owned turn emitter.
            event_emitter: Some(Arc::new({
                let emit = permission_emit.clone();
                move |event: JsObject| {
                    let mut frame = JsObject::new();
                    frame.insert("type".into(), "tool_progress".into());
                    for key in ["toolCallId", "toolName", "message", "createdAt"] {
                        frame.insert(
                            key.into(),
                            event.get(key).cloned().unwrap_or(JsValue::Undefined),
                        );
                    }
                    if let Some(value) = event
                        .get("metadata")
                        .filter(|v| crate::runtime::turn::submit::truthy(v))
                    {
                        frame.insert("metadata".into(), value.clone());
                    }
                    let _ = emit(JsValue::Object(frame));
                }
            })),
            user_dialog,
            file_update_notifier: sdk_file_notifier,
            subagent: fork_turn.as_ref().map(|turn| {
                Arc::new(super::agent::ForkPort(turn.clone())) as Arc<dyn OneShotSubagent>
            }),
            plan_files: self
                .plan_files
                .clone()
                .map(|v| v as Arc<dyn crate::host::modules::tool_context::PlanFiles>),
            permission_mode: Some(Arc::new(move || {
                Some(
                    mode_owner
                        .0
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .0
                        .mode
                        .as_js_string()
                        .clone(),
                )
            })),
            goal: Some(Arc::new(self.goal.clone())),
            bind_interaction_profile: Some(Arc::new(move |profile| {
                profile_for_bind.set(profile).map_err(|_| {
                    crate::host::ModuleError::new("Interaction profile is already bound.").into()
                })
            })),
            elicitation_enabled: Some(Arc::new(move || {
                profile_for_presence
                    .get()
                    .is_some_and(|profile| profile.question_provider != "disabled")
            })),
            elicitation: self
                .fork_scope
                .is_none()
                .then(|| Arc::new(elicitation) as capability::RuntimeContext),
            clock: Some(Arc::new(|| JsValue::String(SystemExecutionClock.now_iso()))),
            ..Default::default()
        };
        let sink = Durable {
            recorder,
            session: identity.session_id.clone(),
            turn: identity.turn_id.clone(),
        };
        let message_sink = sink.clone();
        let mut turn_hooks = self.plugin_hooks.clone();
        if let Some(sdk) = &sdk_state {
            for (event, matchers) in &sdk.settings {
                if event.equals_str("ConfigChange") || event.equals_str("FileChanged") {
                    continue;
                }
                if let Some((_, existing)) = turn_hooks.iter_mut().find(|(name, _)| name == event) {
                    existing.extend(matchers.iter().cloned());
                } else {
                    turn_hooks.push((event.clone(), matchers.clone()));
                }
            }
        }
        // One budget per submit, shared by turn and session projections.
        let projection_budget =
            Arc::new(super::assembly::sdk_hooks::HookProjectionBudget::default());
        let native_async_turn = self.forks.as_ref().map(|registry| {
            registry.capture_async_turn(&identity.session_id, self.pinned_steer.clone())
        });
        if let Some(sdk) = &mut sdk_state {
            sdk.native_steer_bound = native_async_turn
                .as_ref()
                .and_then(Option::as_ref)
                .map(|capture| capture.bind_steer.clone());
        }
        let hook_options = crate::runtime::hooks::HookRuntimeOptions {
            on_projection: Some(Arc::new(super::assembly::sdk_hooks::NativeHookProjection {
                session: identity.session_id.clone(),
                sdk: sdk_state.as_ref().is_some_and(|sdk| sdk.include_events),
                emit: permission_emit.clone(),
                error_emit: permission_emit.clone(),
                signal: signal.clone(),
                budget: projection_budget.clone(),
            })),
            on_async_registered: sdk_state.as_ref().filter(|sdk| sdk.active).map(|sdk| {
                if let Some(parent) = self
                    .fork_scope
                    .as_ref()
                    .and_then(|scope| scope.sdk_async_registration.clone())
                {
                    // TS children share the parent's session lifecycle. The
                    // actual child HookRuntime owns this dispatch/cancellation;
                    // do not borrow the creating parent's expired turn guard.
                    return parent;
                }
                Arc::new(super::assembly::sdk_hooks::NativeAsyncRegistration {
                    session: identity.session_id.clone(),
                    include_events: sdk.include_events,
                    registry: sdk.async_registry.clone(),
                    steer: self.pinned_steer.clone(),
                    current_turn: sdk.current_turn.clone(),
                    native_turn: native_async_turn.clone(),
                })
                    as Arc<dyn crate::runtime::hooks::runtime::AsyncHookRegistrationHandler>
            }),

            #[cfg(unix)]
            shell: self.native_sandbox.as_ref().map(|sandbox| {
                Arc::new(crate::runtime::hooks::exec::SandboxedHookShell {
                    sandbox: sandbox.clone(),
                    workspace: self.execution_cwd(),
                }) as Arc<dyn crate::runtime::hooks::exec::ShellPort>
            }),
            http: Some(
                crate::runtime::hooks::exec::HttpHookExecutor::with_native_proxy(
                    self.hook_proxy.as_ref(),
                    &self.hook_env,
                ),
            ),
            ..Default::default()
        };

        if let Some(turn) = &fork_turn {
            turn.bind_detached_hooks(
                turn_hooks.clone(),
                hook_options.clone(),
                sdk_state.clone(),
                self.pinned_steer.clone(),
            )
            .expect("native detached hooks are bound once");
        }
        let mut session_hook_options = hook_options.clone();
        session_hook_options.on_projection =
            Some(Arc::new(super::assembly::sdk_hooks::NativeHookProjection {
                session: identity.session_id.clone(),
                sdk: sdk_state.as_ref().is_some_and(|sdk| sdk.include_events),
                emit: self
                    .session_emit
                    .clone()
                    .unwrap_or_else(|| permission_emit.clone()),
                error_emit: permission_emit.clone(),
                signal: signal.clone(),
                budget: projection_budget,
            }));
        SessionPorts {
            model: RouterModelPortOptions {
                is_main_agent: self.fork_scope.is_none(),
                managed_model_policy: authority
                    .as_ref()
                    .and_then(|a| a.sdk_models.as_ref())
                    .and_then(|m| m.policy.clone()),
                fallback_models: authority
                    .as_ref()
                    .and_then(|a| a.sdk_models.as_ref())
                    .map(|m| m.fallback.clone())
                    .unwrap_or_default(),
                project_path: Some(self.options.project.to_string_lossy().into_owned().into()),
                agent_model: self
                    .config
                    .get("provider")
                    .and_then(JsValue::as_string)
                    .zip(self.config.get("model").and_then(JsValue::as_string))
                    .map(|(p, m)| (p.clone(), m.clone())),
                agent_max_context_tokens: self.config.get("maxContextTokens").cloned(),
            },
            recorder: None,
            tool_results_dir: self.tool_results_dir(&identity.session_id),
            context_runtime: self.context_runtime.clone(),
            context_identity: ContextRequestIdentity::new(binding.clone(), tool_ports.clone()),
            context_config: config,
            tool_execution: execution,
            tool_context: Arc::new(CapabilityContext {
                builder: ToolContextBuilder::new(binding, tool_ports, true),
                plan_todo: plan_todo
                    .clone()
                    .map(|v| v as Arc<dyn crate::host::modules::tool_context::SessionService>),
            }),
            tool_observer: self.permission.clone(),
            plan_todo: plan_todo
                .map(|v| v as Arc<dyn crate::host::modules::capability::PlanTodoHandler>),
            turn: Default::default(),
            interaction: InteractionTurnInput {
                permission: PermissionTurnOptions {
                    profile: RuntimeProfile::resolve(
                        &JsValue::Undefined,
                        &RuntimeProfileOverrides::default(),
                    )
                    .interaction,
                    context: turn_context,
                    rules: turn_rules.clone(),
                    coordinator: self.interactions.clone(),
                    emit: permission_emit.clone(),
                    timeout: Duration::from_secs(30),
                },
                hooks: turn_hooks,
                sdk_hook_state: sdk_state.clone(),
                sdk_file_slot,
                hook_options: Some(hook_options),
                session_hook_options: Some(session_hook_options),
                contribution: None,
                base_env: Some(self.env.clone()),
                signal,
                steer_recorder: Some(Arc::new(sink)),
                record_message: Some(Arc::new(move |message| message_sink.message(message))),
                messages: vec![],
                switches: CompositionSwitches {
                    plan_todo: false,
                    elicitation_available: Some(self.fork_scope.is_none()),
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
    fn current_mode(&self) -> Option<crate::config::permission::PermissionMode> {
        Some(
            self.0
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .0
                .mode
                .clone(),
        )
    }
    fn apply_mode_effect(&self, result: &JsObject) {
        self.apply_result(result);
    }
    fn set_trusted_mode(&self, mode: crate::config::permission::PermissionMode) {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).0.mode = mode;
    }
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
impl Permissions {
    fn apply_result(&self, result: &JsObject) {
        if result.get("type") != Some(&"success".into()) {
            return;
        }
        let Some(mode) = result
            .get("data")
            .and_then(|v| v.get("requestedMode"))
            .filter(|v| permission::is_permission_mode(v))
            .and_then(JsValue::as_string)
        else {
            return;
        };
        let mut state = self.0.lock().unwrap_or_else(|e| e.into_inner());
        let mut mode = crate::config::permission::PermissionMode::from(mode.clone());
        if mode.as_js_string().equals_str("plan") && !state.0.mode.as_js_string().equals_str("plan")
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
}
impl capability::CapabilityResultObserver for Permissions {
    fn on_capability_results(&self, results: Vec<JsObject>) -> HostFuture<()> {
        for result in &results {
            self.apply_result(result);
        }
        Box::pin(async { Ok(()) })
    }
    fn on_results_after_mode_effects(&self, _results: Vec<JsObject>) -> HostFuture<()> {
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

#[cfg(test)]
pub(super) fn native_tools(
    allowed: Option<Vec<JsString>>,
    denied: Option<Vec<JsString>>,
    lsp: Arc<super::families::DefaultLsp>,
    mcp: &SessionMcpTools,
) -> ToolRegistry {
    native_tools_profile(true, allowed, denied, lsp, mcp)
}
fn native_tools_profile(
    web_search: bool,
    allowed: Option<Vec<JsString>>,
    denied: Option<Vec<JsString>>,
    lsp: Arc<super::families::DefaultLsp>,
    mcp: &SessionMcpTools,
) -> ToolRegistry {
    #[cfg(not(unix))]
    let _ = web_search;
    native_tools_with_definitions(allowed, denied, lsp, mcp, {
        let mut definitions = builtin::read_only_tools();
        definitions.extend(builtin::write_tools());
        #[cfg(unix)]
        definitions.extend(builtin::shell_tools());
        #[cfg(unix)]
        definitions.push(Arc::new(
            builtin::execute_code::ExecuteCodeTool::with_sandbox_and_web_search(None, web_search),
        ));
        definitions
    })
}
#[cfg(unix)]
fn native_tools_with_sandbox(
    web_search: bool,
    allowed: Option<Vec<JsString>>,
    denied: Option<Vec<JsString>>,
    lsp: Arc<super::families::DefaultLsp>,
    sandbox: Arc<crate::runtime::tools::sandbox::NativeSandbox>,
    mcp: &SessionMcpTools,
) -> ToolRegistry {
    let mut definitions = builtin::read_only_tools();
    definitions.extend(builtin::write_tools());
    definitions.push(Arc::new(
        builtin::execute_code::ExecuteCodeTool::with_sandbox_and_web_search(
            Some(sandbox.clone()),
            web_search,
        ),
    ));
    definitions.push(Arc::new(builtin::bash::BashTool::with_options(
        builtin::bash::BashToolOptions {
            runner: Some(Arc::new(
                builtin::bash::runner::SandboxedShellCommandRunner(sandbox),
            )),
            ..Default::default()
        },
    )));
    native_tools_with_definitions(allowed, denied, lsp, mcp, definitions)
}
fn native_tools_with_definitions(
    allowed: Option<Vec<JsString>>,
    denied: Option<Vec<JsString>>,
    lsp: Arc<super::families::DefaultLsp>,
    mcp: &SessionMcpTools,
    definitions: Vec<Arc<dyn crate::runtime::tools::ExecutableTool>>,
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
    // Workspace mutation and foreground shell execution share the same host
    // permission gate as read tools; registration never authorizes execution.
    for tool in definitions {
        tools
            .register(tool)
            .expect("native builtin names are unique");
    }
    for tool in builtin::goal::tools() {
        tools
            .register(tool)
            .expect("native goal tool names are unique");
    }
    tools
        .register(Arc::new(
            builtin::ask_user_question::AskUserQuestion::default(),
        ))
        .expect("native question tool name is unique");
    tools
        .register(Arc::new(
            builtin::structured::StructuredOutputTool::default(),
        ))
        .expect("native structured output name is unique");
    tools
        .register(Arc::new(LspTool::new(lsp)))
        .expect("native LSP name is unique");
    // #1433: the session's MCP tools join the same registry (and therefore
    // the same allowed/denied policy and permission layer) after the local
    // entries.
    mcp.register(&tools);
    tools
}

#[cfg(test)]
#[path = "tools_permission_tests.rs"]
mod permission_turn_tests;
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

    pub(super) fn append_fork(&mut self, tools: Vec<Arc<McpBridgeTool>>) {
        self.per_session.extend(tools);
    }

    pub fn is_empty(&self) -> bool {
        self.shared.is_empty() && self.per_session.is_empty()
    }

    /// The registration tail of `compose`: shared-lease tools keep existing
    /// names, per-session tools replace them.
    pub fn register(&self, tools: &ToolRegistry) {
        for tool in &self.shared {
            let name = tool.spec().wire_name.clone();
            // `has` applies visibility; registration also checks hidden names.
            // A raw collision is a refusal, not a process-wide panic.
            if !tools.has(&name)
                && let Err(error) = tools.register(tool.clone())
            {
                eprintln!("Skipping shared MCP tool registration: {error}");
            }
        }
        for tool in &self.per_session {
            // Atomic raw-name replacement does not consult policy-filtered
            // visibility and remains correct when two normalized names collide.
            if let Err(error) = tools.register_or_replace(tool.clone()) {
                eprintln!("Skipping session MCP tool registration: {error}");
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
    executor: Option<tokio::runtime::Handle>,
}
impl Drop for SessionMcpRelease {
    fn drop(&mut self) {
        let per_session = self.per_session.take();
        let shared = self.shared.take();
        if let Some(executor) = self.executor.take() {
            drop(executor.spawn(async move {
                let _ = release_mcp_parts(per_session, shared).await;
            }));
        }
    }
}
impl SessionMcpRelease {
    /// Release both obligations. The first failure still releases the second,
    /// and both failures surface together.
    pub async fn release(mut self) -> Result<(), RuntimeError> {
        let per_session = self.per_session.take();
        let shared = self.shared.take();
        if let Some(executor) = self.executor.take() {
            return executor
                .spawn(release_mcp_parts(per_session, shared))
                .await
                .map_err(|error| RuntimeError::new(error.to_string()))?;
        }
        release_mcp_parts(per_session, shared).await
    }
}
async fn release_mcp_parts(
    per_session: Option<SessionMcpRuntimeRegistration>,
    shared: Option<ProjectMcpRuntimeLease>,
) -> Result<(), RuntimeError> {
    let mut failures = Vec::new();
    if let Some(registration) = per_session
        && let Err(error) = registration.dispose().await
    {
        failures.push(error);
    }
    if let Some(lease) = shared
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

/// #1433: inputs of [`session_mcp_tools`] (`SessionMcpRuntimeBundleOptions`).
pub struct SessionMcpToolsOptions<'a> {
    /// The SDK-owned session server table (`set_mcp_servers` commits here).
    pub mcp_config: &'a SdkMcpConfig,
    /// The same frozen plugin generation used by this session's skills/hooks.
    /// Enabled SDK entries override these declarations, as in the TS bundle.
    pub plugin_servers: Option<&'a JsObject>,
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
    let mut combined = options.plugin_servers.cloned().unwrap_or_default();
    for (name, config) in servers.own_entries() {
        combined.insert(name.clone(), config.clone());
    }
    let servers = combined;
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
        executor: Some(tokio::runtime::Handle::current()),
    };
    let Some(specs) = options.provider.per_session_server_specs(&servers, false)? else {
        return Ok((tools, release));
    };
    let server_list = specs
        .iter()
        .map(|spec| spec.id.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    let limit_diagnostic = || {
        format!(
            "Per-session MCP limit reached ({}). Session {session_key} will not start: {}.",
            options.max_per_session_instances, server_list,
        )
    };
    // The TS check (SessionMcpRuntimeBundle.ts:70) refuses before the runtime
    // factory runs, so the pre-check stays ahead of `create_runtime`.
    if options.per_session_runtimes.size() as f64 >= options.max_per_session_instances {
        diagnostic(limit_diagnostic(), None);
        return Ok((tools, release));
    }
    let runtime = options.provider.create_runtime(specs);
    // #2442: the pre-check above and the insertion below are two separate
    // lock acquisitions; another session's concurrent prepare can register
    // between them, so the cap is re-checked and the entry inserted in one
    // critical section (`register_below_cap`).
    // Registration precedes the start (SessionMcpRuntimeBundle.ts:119-125):
    // even a failed start is disposed at close, never leaked.
    let registration = match options.per_session_runtimes.register_below_cap(
        &session_key,
        runtime.clone(),
        options.max_per_session_instances,
    ) {
        Ok(Some(registration)) => registration,
        Ok(None) => {
            diagnostic(limit_diagnostic(), None);
            return Ok((tools, release));
        }
        Err(error) => return Err(error),
    };
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

pub(super) async fn fork_mcp_tools(
    provider: &ProjectMcpRuntimeProvider,
    registry: &SessionMcpRuntimeRegistry,
    session: &JsString,
    servers: &JsObject,
    definition: &JsObject,
) -> Result<(Vec<Arc<McpBridgeTool>>, SessionMcpRelease), RuntimeError> {
    let specs = super::agent_mcp::server_specs(servers)
        .map_err(|error| RuntimeError::new(error.message))?;
    let runtime = provider.create_runtime(specs);
    let registration =
        registry.register(&String::from_utf16_lossy(session.units()), runtime.clone())?;
    let release = SessionMcpRelease {
        per_session: Some(registration),
        shared: None,
        executor: Some(tokio::runtime::Handle::current()),
    };
    let result = async {
        runtime.start().await?;
        create_mcp_tool_definitions_from_runtime(runtime, CreateToolDefinitionsOptions::default())
            .await
    }
    .await;
    match result {
        Ok(tools) => Ok((
            tools
                .into_iter()
                .filter(|tool| super::agent::definition_allows(definition, &tool.spec().wire_name))
                .collect(),
            release,
        )),
        Err(error) => match release.release().await {
            Ok(()) => Err(error),
            Err(cleanup) => Err(RuntimeError::new(format!(
                "{}; child MCP cleanup failed: {}",
                error.message, cleanup.message
            ))),
        },
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
        fn plugins<'a>(
            &'a self,
            _: &'a JsValue,
        ) -> crate::runtime::mcp::sdk::SdkMcpFuture<
            'a,
            Vec<Arc<crate::runtime::plugins::LoadedPlugin>>,
        > {
            Box::pin(async move { Vec::new() })
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

    struct PendingStartRuntime {
        entered: Arc<std::sync::atomic::AtomicBool>,
        stopped: Arc<std::sync::atomic::AtomicUsize>,
    }
    impl crate::runtime::mcp::runtime::SessionMcpRuntime for PendingStartRuntime {
        fn stop(&self) -> McpFuture<'_, Result<(), RuntimeError>> {
            self.stopped
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Box::pin(async { Ok(()) })
        }
    }
    impl McpRuntimePort for PendingStartRuntime {
        fn start(&self) -> McpFuture<'_, Result<Vec<McpClientStatusEntry>, RuntimeError>> {
            self.entered
                .store(true, std::sync::atomic::Ordering::SeqCst);
            Box::pin(std::future::pending())
        }
        fn list_all_tools(&self) -> McpFuture<'_, Result<Vec<McpToolSpec>, RuntimeError>> {
            Box::pin(async { Ok(vec![]) })
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
            Box::pin(async { Err(McpClientError::new("unused", None, "unused")) })
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
            plugin_servers: None,
            provider,
            per_session_runtimes: runtimes,
            session_key: &JsString::from(session_key),
            max_per_session_instances,
            on_diagnostic,
        })
        .await
    }

    #[tokio::test]
    async fn plugin_mcp_servers_join_session_tools_with_sdk_precedence() {
        let config = test_config();
        config
            .set_mcp_servers(&set_input("s", &[("collision", true), ("disabled", true)]))
            .await
            .unwrap();
        config
            .toggle_mcp_server(&toggle_input("s", "disabled", false))
            .await
            .unwrap();
        let declarations = set_input(
            "s",
            &[
                ("plugin_shared", false),
                ("plugin_private", true),
                ("collision", false),
                ("disabled", false),
            ],
        );
        let plugins = declarations.get("servers").unwrap().as_object().unwrap();
        let events = Arc::new(Mutex::new(Vec::new()));
        let provider = fake_provider(events.clone(), false);
        let runtimes = SessionMcpRuntimeRegistry::new();
        let (tools, release) = session_mcp_tools(SessionMcpToolsOptions {
            mcp_config: &config,
            plugin_servers: Some(plugins),
            provider: &provider,
            per_session_runtimes: &runtimes,
            session_key: &"s".into(),
            max_per_session_instances: 5.0,
            on_diagnostic: None,
        })
        .await
        .unwrap();
        let names = |tools: &[Arc<McpBridgeTool>]| {
            tools
                .iter()
                .map(|tool| tool.spec().wire_name.clone())
                .collect::<Vec<_>>()
        };
        assert_eq!(
            names(tools.shared()),
            vec![
                JsString::from("mcp__plugin_shared__echo"),
                "mcp__disabled__echo".into()
            ]
        );
        assert_eq!(
            names(tools.per_session()),
            vec![
                JsString::from("mcp__plugin_private__echo"),
                "mcp__collision__echo".into()
            ]
        );
        assert_eq!(runtimes.size(), 1);
        release.release().await.unwrap();
        assert_eq!(runtimes.size(), 0);
        provider.dispose().await.unwrap();
        assert_eq!(
            *events.lock().unwrap(),
            vec!["stop:per-session", "stop:shared"]
        );
    }

    #[tokio::test]
    async fn cancelling_pending_session_start_releases_registration_and_stops_runtime() {
        use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
        let config = test_config();
        config
            .set_mcp_servers(&set_input("s", &[("private", true)]))
            .await
            .unwrap();
        let entered = Arc::new(AtomicBool::new(false));
        let stopped = Arc::new(AtomicUsize::new(0));
        let factory_entered = entered.clone();
        let factory_stopped = stopped.clone();
        let provider = Arc::new(ProjectMcpRuntimeProvider::new(
            ProjectMcpRuntimeProviderOptions {
                project_root: "/pdrs-fixture/project".into(),
                pilot_home: "/pdrs-fixture/home".into(),
                create_runtime: Some(Arc::new(move |_| {
                    Arc::new(PendingStartRuntime {
                        entered: factory_entered.clone(),
                        stopped: factory_stopped.clone(),
                    }) as Arc<dyn McpRuntimePort>
                })),
                proxy: None,
                on_diagnostic: None,
                environment: Arc::new(SystemEnvironment),
                builtin_paths: BuiltinMcpPaths::default(),
            },
        ));
        let registry = SessionMcpRuntimeRegistry::new();
        let task_registry = registry.clone();
        let task = tokio::spawn(async move {
            compose(&config, &provider, &task_registry, "s", 5.0, None).await
        });
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            while !entered.load(Ordering::SeqCst) {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert_eq!(registry.size(), 1);
        task.abort();
        assert!(matches!(task.await, Err(error) if error.is_cancelled()));
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            while registry.size() != 0 || stopped.load(Ordering::SeqCst) != 1 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("cancelled startup must release its slot and stop exactly once");
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
    /// #2444: both halves are actually observed. The registry dispose is
    /// awaited while both stops are still gated, and the shared gate only
    /// opens after the shared stop has parked on it — so the provider's
    /// dispose is provably parked on an awaited stop, stays unfinished until
    /// the gate opens, and the provider's state lock is probed during that
    /// parked stop (a lock held across the stop's await surfaces as the
    /// probe's timeout).
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
            executor: Some(tokio::runtime::Handle::current()),
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
        let registry_dispose = {
            let runtimes = runtimes.clone();
            tokio::spawn(async move { runtimes.dispose().await })
        };
        let mut provider_dispose = {
            let provider = provider.clone();
            tokio::spawn(async move { provider.dispose().await })
        };
        // #2444: the registry dispose drains only the "t" entry (the release
        // already unlinked its own), so it must finish while both stops are
        // still gated — awaited before the gates open, making the "not
        // blocked by the parked release" claim an actual observation.
        let registry_result =
            tokio::time::timeout(std::time::Duration::from_secs(2), registry_dispose)
                .await
                .expect("#2444 registry dispose completes while both stops are gated")
                .expect("#2444 the registry dispose task joins");
        crate::test_negative_receipt::expect_ok(registry_result, "#1433 registry dispose returns");
        open_per_session
            .send(())
            .expect("#2444 open the per-session stop gate");
        // #2444: wait for the release to hand its shared lease back — the
        // shared stop pushes its label before awaiting the gate, so from here
        // the provider's dispose settle is parked on an actually-awaited stop.
        let mut shared_parked = false;
        for _ in 0..400 {
            if events
                .lock()
                .expect("#2444 events lock")
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
            "#2444 the shared stop parks on its gate before the provider settle waits"
        );
        // #2444: with the shared stop parked on its gate, the provider dispose
        // cannot have settled — assert it stays unfinished BEFORE opening the
        // gate (300 ms budget).
        crate::negative_assert!(
            tokio::time::timeout(std::time::Duration::from_millis(300), &mut provider_dispose)
                .await
                .is_err(),
            "#2444 the provider dispose stays parked at the gated shared stop"
        );
        // #2444: the provider half's observation window — the parked shared
        // stop must not hold the provider's state lock (the probe runs as
        // its own task, so a held lock surfaces as a timeout, not a blocked
        // worker).
        let shared_probe = {
            let provider = provider.clone();
            tokio::spawn(async move { provider.shared_generation_count() })
        };
        let shared_generations =
            tokio::time::timeout(std::time::Duration::from_secs(2), shared_probe)
                .await
                .expect("#2444 the provider stays lockable during the gated shared stop")
                .expect("#2444 the shared-stop lockability probe joins");
        crate::negative_assert_eq!(
            shared_generations,
            1,
            "#2444 the parked shared stop keeps its entry until the stop settles"
        );
        open_shared
            .send(())
            .expect("#2444 open the shared stop gate");
        let released = tokio::time::timeout(std::time::Duration::from_secs(2), release_task)
            .await
            .expect("#1433 the release finishes once the gates open")
            .expect("#1433 the release task joins");
        crate::test_negative_receipt::expect_ok(released, "#1433 gated release returns");
        let provider_result =
            tokio::time::timeout(std::time::Duration::from_secs(2), provider_dispose)
                .await
                .expect("#2444 the provider dispose finishes once the gate opens")
                .expect("#2444 the provider dispose task joins");
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

    /// #2442: the size pre-check and the registry insertion are separate
    /// lock acquisitions, so a concurrent session's compose can register
    /// between them. The gated factory parks session-a's compose after its
    /// pre-check (size 4 < cap 5) and before its registration; session-b's
    /// compose registers the fifth runtime in the meantime; the sixth must
    /// still be refused (`register_below_cap` re-checks the cap under the
    /// registry's own lock). Unfixed, both register and the size ends at 6.
    /// The owner runs creations for different session keys as independent
    /// tasks (see the factory overlap probe), so the interleave is reachable
    /// from the production path.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn session_mcp_tools_refuses_the_sixth_runtime_when_a_racer_registers_after_the_check() {
        use std::{
            sync::atomic::{AtomicBool, Ordering},
            time::{Duration, Instant},
        };
        let config = test_config();
        for key in ["session-a", "session-b"] {
            config
                .set_mcp_servers(&set_input(
                    key,
                    &[("shared-echo", false), ("private-echo", true)],
                ))
                .await
                .expect("#2442 fixture servers commit");
        }
        let events: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let runtimes = SessionMcpRuntimeRegistry::new();
        for key in ["p1", "p2", "p3", "p4"] {
            runtimes
                .register(
                    key,
                    Arc::new(FakeMcpRuntime {
                        label: "per-session",
                        tools: Vec::new(),
                        start_fails: false,
                        events: events.clone(),
                    }),
                )
                .expect("#2442 pre-existing per-session runtime");
        }
        // Session-a's provider: the per-session factory call (private-echo)
        // parks until the racing registration has landed (size reaches 5),
        // holding the compose between its pre-check and its register; the
        // shared call (shared-echo) returns immediately. In production both
        // composes share one provider; only the registry is the shared race
        // surface, so the racer below uses its own provider.
        let entered = Arc::new(AtomicBool::new(false));
        let racing_registry = runtimes.clone();
        let entered_flag = entered.clone();
        let gated_events = events.clone();
        let gated_provider = Arc::new(ProjectMcpRuntimeProvider::new(
            ProjectMcpRuntimeProviderOptions {
                project_root: "/pdrs-fixture/project".into(),
                pilot_home: "/pdrs-fixture/home".into(),
                create_runtime: Some(Arc::new(move |specs: Vec<McpServerSpec>| {
                    let per_session = specs.iter().any(|spec| spec.id == "private-echo");
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
                                    .expect("#2442 fake tool schema"),
                                annotations: None,
                                meta: None,
                            })
                            .collect(),
                        start_fails: false,
                        events: gated_events.clone(),
                    });
                    if !per_session {
                        return runtime;
                    }
                    entered_flag.store(true, Ordering::SeqCst);
                    let deadline = Instant::now() + Duration::from_secs(10);
                    while racing_registry.size() < 5 {
                        if Instant::now() > deadline {
                            panic!("#2442 the racing registration never landed");
                        }
                        std::thread::sleep(Duration::from_millis(5));
                    }
                    runtime
                })),
                proxy: None,
                on_diagnostic: None,
                environment: Arc::new(SystemEnvironment),
                builtin_paths: BuiltinMcpPaths::default(),
            },
        ));
        let diagnostics: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let sink = diagnostics.clone();
        let gated_config = config.clone();
        let gated_runtimes = runtimes.clone();
        let compose_a = tokio::spawn(async move {
            compose(
                &gated_config,
                &gated_provider,
                &gated_runtimes,
                "session-a",
                5.0,
                Some(Arc::new(move |message: String, _: Option<RuntimeError>| {
                    sink.lock().expect("#2442 diagnostics lock").push(message);
                })),
            )
            .await
        });
        // Wait until session-a is parked inside its per-session factory call —
        // past the pre-check (it read size 4) and before its registration.
        let mut parked = false;
        for _ in 0..400 {
            if entered.load(Ordering::SeqCst) {
                parked = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        crate::negative_assert!(
            parked,
            "#2442 the gated compose parks between its pre-check and its registration"
        );
        // The racer: a full compose for another session, whose registration
        // is the fifth runtime (its own pre-check still reads 4).
        let racing_provider = fake_provider(events.clone(), false);
        let (racing_tools, racing_release) = crate::test_negative_receipt::expect_ok(
            compose(&config, &racing_provider, &runtimes, "session-b", 5.0, None).await,
            "#2442 the racing compose succeeds",
        );
        crate::negative_assert!(
            racing_tools
                .per_session()
                .iter()
                .any(|tool| wire_name(tool) == "mcp__private-echo__echo"),
            "#2442 the racing compose registers the fifth runtime"
        );
        // The gated compose resumes and must be refused past the cap.
        let gated = tokio::time::timeout(Duration::from_secs(10), compose_a)
            .await
            .expect("#2442 the gated compose finishes once the racer registered")
            .expect("#2442 the gated compose task joins");
        let (gated_tools, gated_release) =
            crate::test_negative_receipt::expect_ok(gated, "#2442 the gated compose returns");
        crate::negative_assert!(
            gated_tools.per_session().is_empty(),
            "#2442 the over-cap compose starts no per-session runtime"
        );
        crate::negative_assert!(
            gated_tools
                .shared()
                .iter()
                .any(|tool| wire_name(tool) == "mcp__shared-echo__echo"),
            "#2442 the refusal keeps the shared tools"
        );
        crate::negative_assert!(
            diagnostics
                .lock()
                .expect("#2442 diagnostics lock")
                .iter()
                .any(|message| message.starts_with(
                    "Per-session MCP limit reached (5). Session session-a will not start: private-echo."
                )),
            "#2442 the refusal reports the TS diagnostic"
        );
        crate::negative_assert_eq!(
            runtimes.size(),
            5,
            "#2442 the racing registration stands and the sixth runtime is refused"
        );
        racing_release
            .release()
            .await
            .expect("#2442 racing release");
        gated_release
            .release()
            .await
            .expect("#2442 the refused compose releases its shared lease");
        crate::negative_assert_eq!(
            runtimes.size(),
            4,
            "#2442 only the four pre-existing runtimes remain"
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

    #[test]
    fn policy_hidden_colliding_mcp_names_never_panic() {
        let same = bridge_tool("mcp__a_b__echo");
        let composed = SessionMcpTools {
            shared: vec![same.clone(), same.clone()],
            per_session: vec![same],
        };
        let hidden = native_tools(
            None,
            Some(vec!["mcp__a_b__echo".into()]),
            Arc::new(super::super::families::DefaultLsp::default()),
            &composed,
        );
        assert!(!hidden.has(&"mcp__a_b__echo".into()));
        assert!(hidden.has(&"read_file".into()));
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
