//! `LiveSession` over one Module 2.0 sidecar execution (A7.2-pr6).
//!
//! `submit` encodes the execute request, spawns the sidecar, and turns the
//! callback/event stream into a `LiveTurn` of AgentEvents. Native terminal is
//! preserved; `abort` cancels the bound `CallControl` so an active turn
//! settles instead of hanging (handle.rs:39 contract).
use super::handle::{LiveSession, LiveTurn, SessionNext};
use crate::host::{
    Cancellation, ExecuteRequest, ExecuteResult, HostClient, HostError, HostFuture, HostHandler,
    Limits, ModuleError,
};
use crate::runtime::project::{DisposeFuture, RuntimeError, RuntimeResult};
use crate::runtime::steer::{
    CancelResult, EnqueueResult, MutationRecorder, SteerMailbox, SteerMessage, SteerReason,
    TurnSteers,
};
use crate::runtime::turn::execute_payload::{ExecutePayloadInput, encode_execute};
use crate::storage::TranscriptWriter;
use pilotdeck_protocol::{JsObject, JsString, JsValue, stringify_json};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
use tokio::sync::{mpsc, watch};

/// Builds the sidecar command for one turn. Kept as a port so tests and the
/// CLI/Gateway wiring can differ without the session knowing how to spawn.
pub trait SidecarSpawner: Send + Sync {
    fn command(&self) -> std::io::Result<tokio::process::Command>;
    /// Test and capture spawners must run the real stdio sidecar path; production
    /// [`BinarySidecarSpawner`](crate::runtime::turn::wiring::BinarySidecarSpawner) stays embedded unless
    /// `PILOTDECK_AGENT_SUBPROCESS=1`.
    fn requires_subprocess(&self) -> bool {
        false
    }
    fn connect(&self, limits: Limits) -> crate::host::HostFuture<Option<HostClient>> {
        let use_subprocess =
            self.requires_subprocess() || super::embedded_sidecar::subprocess_sidecar_enabled();
        let command = use_subprocess.then(|| self.command());
        Box::pin(async move {
            let Some(command) = command else {
                return Ok(None);
            };
            let mut command = command.map_err(|error| {
                HostError::new(crate::host::HostErrorKind::Io, error.to_string())
            })?;
            HostClient::spawn(&mut command, Some(&"pilotdeck-agent-loop".into()), limits)
                .await
                .map(Some)
        })
    }
}

pub struct SidecarSessionConfig {
    pub limits: Limits,
    pub session_key: JsString,
    pub project_key: Option<JsString>,
    /// Turn-independent host state the tool/model ports read.
    pub cwd: JsString,
    /// Already-loaded configuration snapshot shared by every turn.
    pub snapshot: JsValue,
    pub services: Option<Arc<crate::runtime::turn::services::NativeSessionServices>>,
}

/// One gateway session backed by a fresh sidecar process per turn. TS also
/// spawns per turn for stdio (`stdioAgentLoopSidecarConnection`); a retained
/// process is a separate deployment profile and is not assumed here.
pub struct SidecarSession {
    subagent_providers: Option<
        Arc<crate::runtime::session::subagent_continuation::NamedContinuationProviderRegistry>,
    >,
    mcp_owner: Option<Arc<crate::runtime::manager::session_mcp::SessionMcpOwner>>,
    user_dialogs: Option<Arc<crate::user_dialog::GatewayUserDialogs>>,
    user_dialog_kinds: Vec<JsString>,
    config: SidecarSessionConfig,
    spawner: Arc<dyn SidecarSpawner>,
    active: Arc<Mutex<Option<ActiveTurn>>>,
    persistence: Option<Arc<dyn SteerPersistence>>,
    transcript: Option<Arc<TranscriptWriter>>,
    usage_stats: Option<crate::runtime::turn::routes::usage::SharedUsageStats>,
    router_state: Arc<crate::runtime::model::router::RouterSharedState>,
    interactions: Option<Arc<crate::runtime::interaction::GatewayInteractionCoordinator>>,
    embedded_worker: Arc<super::embedded_sidecar::SessionEmbeddedWorker>,
    disposed: AtomicBool,
    fork_scope: Option<ForkScope>,
    fork_turn_deadline: Option<tokio::time::Instant>,
    fork_parent_cancellation: Option<crate::runtime::network::Cancellation>,
    fork_router_abort: Option<crate::runtime::network::Cancellation>,
    context_resources: Arc<super::context_resources::ContextResources>,
    owns_context_resources: bool,
    lifecycle: crate::runtime::hooks::LifecycleRuntime,
    include_hook_events: bool,
    async_hook_bindings: Option<crate::runtime::async_hooks::LiveAsyncHookBindings>,
    plugin_lease:
        tokio::sync::Mutex<Option<crate::runtime::plugins::PluginSessionContributionLease>>,
    config_change_lifecycle: Option<crate::runtime::hooks::LifecycleRuntime>,
    file_changed_lifecycle: Option<crate::runtime::hooks::LifecycleRuntime>,
    sdk_handlers: tokio::sync::Mutex<Option<SdkHookHandlers>>,
}

struct SdkHookHandlers {
    registry: Arc<tokio::sync::Mutex<crate::runtime::sdk_session::SdkSessionRegistry>>,
    config: Option<crate::runtime::sdk_session::HandlerRegistration>,
    session: crate::runtime::sdk_session::HandlerRegistration,
}

fn external_hook_dispatch(
    runtime: crate::runtime::hooks::LifecycleRuntime,
    session: JsString,
    cwd: JsString,
) -> Arc<dyn Fn(JsString, JsValue) + Send + Sync> {
    Arc::new(move |event, payload| {
        let runtime = runtime.clone();
        let session = session.clone();
        let cwd = cwd.clone();
        tokio::spawn(async move {
            let mut base = JsObject::new();
            base.insert("sessionId".into(), JsValue::String(session));
            base.insert("transcriptPath".into(), "".into());
            base.insert("cwd".into(), JsValue::String(cwd.clone()));
            let _ = runtime
                .dispatch(crate::runtime::hooks::LifecycleDispatchInput {
                    event: event.clone(),
                    base_input: base,
                    payload: payload.as_object().cloned(),
                    match_query: Some(event),
                    cwd,
                    env: None,
                    signal: None,
                })
                .await;
        });
    })
}

struct ForkScope {
    registry: crate::runtime::tools::ToolRegistry,
    live: Option<Arc<super::routes::live::LiveSessionState>>,
    activity: Arc<dyn Fn(JsValue) -> Result<(), HostError> + Send + Sync>,
    lifecycle: crate::runtime::hooks::LifecycleRuntime,
}

struct ActiveTurn {
    accepting: bool,
    identity: Arc<()>,
    steers: TurnSteers,
    cancellation: Cancellation,
    host_cancellation: crate::runtime::network::Cancellation,
    completion: watch::Receiver<bool>,
    /// Sending `None` ends the event pump.
    sink: mpsc::UnboundedSender<Option<Result<JsValue, RuntimeError>>>,
}

fn is_current(active: &Mutex<Option<ActiveTurn>>, identity: &Arc<()>) -> bool {
    active
        .lock()
        .unwrap()
        .as_ref()
        .is_some_and(|turn| Arc::ptr_eq(&turn.identity, identity))
}

struct ActiveGuard {
    active: Arc<Mutex<Option<ActiveTurn>>>,
    identity: Arc<()>,
    sink: mpsc::UnboundedSender<Option<Result<JsValue, RuntimeError>>>,
    steers: TurnSteers,
    turn_id: JsString,
    completed: watch::Sender<bool>,
    hook_binding: Option<crate::runtime::async_hooks::LiveAsyncHookGuard>,
    hook_subscription: Vec<crate::runtime::hooks::runtime::HookExecutionEventSubscription>,
}

impl Drop for ActiveGuard {
    fn drop(&mut self) {
        // Expiry events must enter the existing stream before its terminal
        // `None` sentinel, while the session binding still identifies this run.
        self.hook_binding.take();
        for subscription in self.hook_subscription.drain(..) {
            subscription.dispose();
        }
        self.steers.mailbox.finish(&self.turn_id);
        let mut active = self.active.lock().unwrap();
        if active
            .as_ref()
            .is_some_and(|turn| Arc::ptr_eq(&turn.identity, &self.identity))
        {
            active.take();
        }
        drop(active);
        self.completed.send_replace(true);
        let _ = self.sink.send(None);
    }
}

/// Both writes use the session's existing transcript owner.
pub trait SteerPersistence: Send + Sync {
    fn mutation(&self, turn_id: JsString, mutation: JsObject) -> HostFuture<()>;
    fn message(&self, turn_id: JsString, message: JsValue) -> HostFuture<()>;
}

struct TranscriptSteers {
    writer: Arc<TranscriptWriter>,
    session_id: JsString,
}

impl SteerPersistence for TranscriptSteers {
    fn mutation(&self, turn_id: JsString, mut mutation: JsObject) -> HostFuture<()> {
        mutation.insert("type".into(), "inbox_mutation".into());
        let result = self
            .writer
            .record_native_session_event(&self.session_id, &turn_id, mutation)
            .map_err(|error| ModuleError::new(error.to_string()).into());
        Box::pin(async move { result })
    }
    fn message(&self, turn_id: JsString, message: JsValue) -> HostFuture<()> {
        let mut draft = JsObject::new();
        draft.insert("type".into(), "durable_message".into());
        draft.insert("message".into(), message);
        let result = self
            .writer
            .record_native_session_event(&self.session_id, &turn_id, draft)
            .map_err(|error| ModuleError::new(error.to_string()).into());
        Box::pin(async move { result })
    }
}

struct SteerRecorder(Option<Arc<dyn SteerPersistence>>);
impl MutationRecorder for SteerRecorder {
    fn record(&self, turn_id: JsString, mutation: JsObject) -> HostFuture<()> {
        match &self.0 {
            Some(persistence) => persistence.mutation(turn_id, mutation),
            None => Box::pin(async {
                Err(ModuleError::new("The active session has no transcript writer.").into())
            }),
        }
    }
}

impl SidecarSession {
    pub fn new(config: SidecarSessionConfig, spawner: Arc<dyn SidecarSpawner>) -> Self {
        let cwd = config.cwd.clone();
        let settings = crate::hooks::config::parse_hooks_config(
            config
                .snapshot
                .get("config")
                .and_then(|value| value.get("hooks"))
                .unwrap_or(&JsValue::Undefined),
        )
        .settings;
        Self {
            config,
            spawner,
            subagent_providers: None,
            mcp_owner: None,
            user_dialogs: None,
            user_dialog_kinds: vec![],
            active: Arc::new(Mutex::new(None)),
            persistence: None,
            transcript: None,
            usage_stats: None,
            router_state: Arc::new(crate::runtime::model::router::RouterSharedState::default()),
            interactions: None,
            embedded_worker: Arc::new(super::embedded_sidecar::SessionEmbeddedWorker::new(
                cwd.clone(),
            )),
            disposed: AtomicBool::new(false),
            fork_scope: None,
            fork_turn_deadline: None,
            fork_parent_cancellation: None,
            fork_router_abort: None,
            context_resources: Arc::new(super::context_resources::ContextResources::empty(
                cwd.to_utf8().unwrap_or_default(),
            )),
            owns_context_resources: false,
            lifecycle: crate::runtime::hooks::LifecycleRuntime::new(
                crate::runtime::hooks::HookRuntime::new(settings),
            ),
            include_hook_events: false,
            async_hook_bindings: None,
            plugin_lease: tokio::sync::Mutex::new(None),
            config_change_lifecycle: None,
            file_changed_lifecycle: None,
            sdk_handlers: tokio::sync::Mutex::new(None),
        }
    }

    pub fn with_transcript(
        config: SidecarSessionConfig,
        spawner: Arc<dyn SidecarSpawner>,
        writer: Arc<TranscriptWriter>,
    ) -> Self {
        let persistence = Arc::new(TranscriptSteers {
            writer: writer.clone(),
            session_id: config.session_key.clone(),
        });
        let mut session = Self::with_steer_persistence(config, spawner, persistence);
        session.transcript = Some(writer);
        session
    }

    pub fn with_steer_persistence(
        config: SidecarSessionConfig,
        spawner: Arc<dyn SidecarSpawner>,
        persistence: Arc<dyn SteerPersistence>,
    ) -> Self {
        let mut session = Self::new(config, spawner);
        session.persistence = Some(persistence);
        session
    }

    pub fn with_router_generation(
        mut self,
        state: Arc<crate::runtime::model::router::RouterSharedState>,
    ) -> Self {
        self.router_state = state;
        self
    }

    pub fn with_usage_stats(
        mut self,
        stats: crate::runtime::turn::routes::usage::SharedUsageStats,
    ) -> Self {
        self.usage_stats = Some(stats);
        self
    }

    pub fn with_context_resources(
        mut self,
        resources: Arc<super::context_resources::ContextResources>,
    ) -> Self {
        self.context_resources = resources;
        self
    }

    pub fn with_owned_context_resources(
        mut self,
        resources: Arc<super::context_resources::ContextResources>,
    ) -> Self {
        self.context_resources = resources;
        self.owns_context_resources = true;
        self
    }

    pub fn with_hook_sources(
        mut self,
        sources: Option<(
            crate::hooks::protocol::HooksSettings,
            Option<JsValue>,
            crate::runtime::plugins::PluginSessionContributionLease,
        )>,
        bindings: crate::runtime::async_hooks::LiveAsyncHookBindings,
    ) -> Self {
        if let Some((settings, config, lease)) = sources {
            self.include_hook_events = config
                .as_ref()
                .and_then(|config| config.get("includeHookEvents"))
                .and_then(JsValue::as_bool)
                == Some(true);
            let sdk_hooks = config
                .as_ref()
                .and_then(|config| config.get("hooks"))
                .is_some_and(|hooks| !matches!(hooks, JsValue::Null | JsValue::Undefined));
            let options = crate::runtime::hooks::HookRuntimeOptions {
                on_async_registered: sdk_hooks.then(|| {
                    Arc::new(crate::runtime::hooks::GatewayAsyncHookRegistrationBridge {
                        registry: bindings.registry.clone(),
                        session_key: self.config.session_key.clone(),
                        include_hook_events: self.include_hook_events,
                    })
                        as Arc<dyn crate::runtime::hooks::runtime::AsyncHookRegistrationHandler>
                }),
                ..Default::default()
            };
            for event in ["ConfigChange", "FileChanged"] {
                let external = crate::runtime::hooks::session::external_sdk_settings(
                    config.as_ref().and_then(|config| config.get("hooks")),
                    event,
                );
                if !external.is_empty() {
                    let runtime = crate::runtime::hooks::LifecycleRuntime::new(
                        crate::runtime::hooks::HookRuntime::with_options(
                            external,
                            crate::runtime::hooks::HookRuntimeOptions {
                                on_async_registered: if event == "FileChanged" {
                                    options.on_async_registered.clone()
                                } else {
                                    None
                                },
                                ..Default::default()
                            },
                        ),
                    );
                    if event == "ConfigChange" {
                        self.config_change_lifecycle = Some(runtime);
                    } else {
                        self.file_changed_lifecycle = Some(runtime);
                    }
                }
            }
            self.lifecycle = crate::runtime::hooks::LifecycleRuntime::new(
                crate::runtime::hooks::HookRuntime::with_options(settings, options),
            );
            self.plugin_lease = tokio::sync::Mutex::new(Some(lease));
        }
        self.async_hook_bindings = Some(bindings);
        self
    }

    pub async fn register_sdk_hook_handlers(
        self,
        registry: Arc<tokio::sync::Mutex<crate::runtime::sdk_session::SdkSessionRegistry>>,
    ) -> Self {
        let mut owner = registry.lock().await;
        let config = self.config_change_lifecycle.as_ref().map(|runtime| {
            let dispatch = external_hook_dispatch(
                runtime.clone(),
                self.config.session_key.clone(),
                self.config.cwd.clone(),
            );
            owner.register_config_change_handler(
                &self.config.session_key,
                Arc::new(move |payload| {
                    dispatch(
                        "ConfigChange".into(),
                        crate::metadata::object([
                            (
                                "changedPaths",
                                JsValue::Array(
                                    payload
                                        .changed_paths
                                        .iter()
                                        .map(|value| value.as_str().into())
                                        .collect(),
                                ),
                            ),
                            (
                                "changeClasses",
                                JsValue::Array(
                                    payload
                                        .change_classes
                                        .iter()
                                        .map(|value| value.as_str().into())
                                        .collect(),
                                ),
                            ),
                        ]),
                    );
                }),
            )
        });
        let dispatch = external_hook_dispatch(
            self.lifecycle.clone(),
            self.config.session_key.clone(),
            self.config.cwd.clone(),
        );
        let session = owner.register_session_hook_handler(
            &self.config.session_key,
            Arc::new(move |event, payload| dispatch(event.as_str().into(), payload.clone())),
        );
        drop(owner);
        *self.sdk_handlers.lock().await = Some(SdkHookHandlers {
            registry,
            config,
            session,
        });
        self
    }

    pub fn with_mcp_owner(
        mut self,
        owner: Option<Arc<crate::runtime::manager::session_mcp::SessionMcpOwner>>,
    ) -> Self {
        self.mcp_owner = owner;
        self
    }

    pub fn with_subagent_providers(
        mut self,
        providers: Arc<
            crate::runtime::session::subagent_continuation::NamedContinuationProviderRegistry,
        >,
    ) -> Self {
        self.subagent_providers = Some(providers);
        self
    }

    pub fn with_session_tools(
        mut self,
        dialogs: Arc<crate::user_dialog::GatewayUserDialogs>,
        kinds: Vec<JsString>,
    ) -> Self {
        self.user_dialogs = Some(dialogs);
        self.user_dialog_kinds = kinds;
        self
    }

    pub fn with_user_dialogs(
        mut self,
        dialogs: Arc<crate::user_dialog::GatewayUserDialogs>,
    ) -> Self {
        self.user_dialogs = Some(dialogs);
        self
    }

    pub fn with_interactions(
        mut self,
        interactions: Arc<crate::runtime::interaction::GatewayInteractionCoordinator>,
    ) -> Self {
        self.interactions = Some(interactions);
        self
    }

    pub(crate) fn with_fork_scope(
        mut self,
        registry: crate::runtime::tools::ToolRegistry,
        live: Option<Arc<super::routes::live::LiveSessionState>>,
        activity: Arc<dyn Fn(JsValue) -> Result<(), HostError> + Send + Sync>,
        lifecycle: crate::runtime::hooks::LifecycleRuntime,
    ) -> Self {
        self.fork_scope = Some(ForkScope {
            registry,
            live,
            activity,
            lifecycle,
        });
        self
    }

    pub fn with_fork_turn_deadline(mut self, deadline: tokio::time::Instant) -> Self {
        self.fork_turn_deadline = Some(deadline);
        self
    }

    pub fn with_fork_parent_cancellation(
        mut self,
        cancellation: crate::runtime::network::Cancellation,
    ) -> Self {
        self.fork_parent_cancellation = Some(cancellation);
        self
    }

    pub fn with_fork_router_abort(
        mut self,
        cancellation: crate::runtime::network::Cancellation,
    ) -> Self {
        self.fork_router_abort = Some(cancellation);
        self
    }
}

impl LiveSession for SidecarSession {
    fn submit(&self, input: JsValue, options: JsValue) -> Result<Box<dyn LiveTurn>, RuntimeError> {
        if self.disposed.load(Ordering::SeqCst) {
            return Err(RuntimeError::new("Agent session is disposed."));
        }
        let mut input = input;
        if let JsValue::Object(fields) = &mut input
            && !fields.contains_key("hostUserDialogKinds")
        {
            fields.insert(
                "hostUserDialogKinds".into(),
                JsValue::Array(
                    self.user_dialog_kinds
                        .iter()
                        .cloned()
                        .map(JsValue::String)
                        .collect(),
                ),
            );
        }
        let registry_input = {
            let mut enriched = input.clone();
            if let JsValue::Object(fields) = &mut enriched {
                if let Some(modules) = self
                    .config
                    .snapshot
                    .get("config")
                    .and_then(|config| config.get("modules"))
                {
                    fields.insert("configModules".into(), modules.clone());
                }
            }
            enriched
        };
        let registry = match &self.fork_scope {
            Some(scope) => Ok(scope.registry.clone()),
            None => {
                super::module_host::with_context_resources(self.context_resources.clone(), || {
                    super::module_host::with_mcp_owner(self.mcp_owner.clone(), || {
                        super::module_host::build_registry(
                            &registry_input,
                            self.config.services.clone(),
                        )
                    })
                })
            }
        }
        .map_err(|e| RuntimeError::new(e.to_string()))?;
        if let JsValue::Object(payload) = &mut input {
            payload.insert(
                "tools".into(),
                JsValue::Array(
                    super::module_host::descriptors(&registry)
                        .map_err(|e| RuntimeError::new(e.to_string()))?,
                ),
            );
        }
        let external = self.fork_scope.is_none();
        let cancellation = Cancellation::default();
        let cancellation_for_host = self
            .fork_router_abort
            .clone()
            .unwrap_or_else(crate::runtime::network::Cancellation::new);
        if self.fork_scope.is_some() && self.fork_router_abort.is_none() {
            if let Some(parent) = self.fork_parent_cancellation.clone() {
                let child = cancellation_for_host.clone();
                tokio::spawn(async move {
                    parent.cancelled().await;
                    child.cancel();
                });
            }
            if let Some(deadline) = self.fork_turn_deadline {
                let child = cancellation_for_host.clone();
                let host = cancellation.clone();
                tokio::spawn(async move {
                    tokio::time::sleep_until(deadline).await;
                    child.cancel();
                    host.cancel(Some("subagent_timeout".into()));
                });
            }
        }
        let turn_id = string_of(&options, "turnId").unwrap_or_default();
        let live = self
            .fork_scope
            .as_ref()
            .and_then(|scope| scope.live.clone())
            .or_else(super::module_host::live_state);
        let persistence = self.persistence.clone().or_else(|| {
            live.as_ref().map(|state| {
                Arc::new(TranscriptSteers {
                    writer: state.recorder.writer().clone(),
                    session_id: self.config.session_key.clone(),
                }) as Arc<dyn SteerPersistence>
            })
        });
        let mailbox = SteerMailbox::new(Some(Arc::new(SteerRecorder(persistence.clone()))));
        mailbox.start(turn_id.clone());
        let steers = TurnSteers::new(mailbox, turn_id.clone(), messages_of(&input));
        let (sink, receiver) = mpsc::unbounded_channel();
        let (completed, completion) = watch::channel(false);
        let identity = Arc::new(());
        let delivery_active = self.active.clone();
        let delivery_identity = identity.clone();
        let delivery_sink = sink.clone();
        let activity = self.fork_scope.as_ref().map(|scope| scope.activity.clone());
        let events = Arc::new(super::module_host::EventBuffer::streaming(Arc::new(
            move |event| {
                if let Some(activity) = &activity {
                    activity(event.clone())?;
                }
                if is_current(&delivery_active, &delivery_identity) {
                    let _ = delivery_sink.send(Some(Ok(event)));
                }
                Ok(())
            },
        )));
        let mut host_input = input.as_object().cloned().unwrap_or_default();
        host_input.insert(
            "sessionId".into(),
            JsValue::String(self.config.session_key.clone()),
        );
        host_input.insert("turnId".into(), JsValue::String(turn_id.clone()));
        if let Some(workspace) = options.get("workspaceId") {
            host_input.insert("workspaceId".into(), workspace.clone());
        }
        host_input.insert(
            "runId".into(),
            options
                .get("execution")
                .and_then(|execution| execution.get("runId"))
                .filter(|value| !matches!(value, JsValue::Null | JsValue::Undefined))
                .cloned()
                .unwrap_or_else(|| JsValue::String(turn_id.clone())),
        );
        let host_input = JsValue::Object(host_input);
        let build_router = || {
            super::module_host::with_context_resources(self.context_resources.clone(), || {
                super::module_host::with_mcp_owner(self.mcp_owner.clone(), || {
                    super::module_host::with_user_dialogs(self.user_dialogs.clone(), || {
                        super::module_host::build_with_router_state(
                            self.config.snapshot.clone(),
                            &host_input,
                            events.clone(),
                            cancellation_for_host.clone(),
                            steers.clone(),
                            self.usage_stats.clone(),
                            self.config.services.clone(),
                            self.interactions.clone(),
                            Some(self.router_state.clone()),
                        )
                    })
                })
            })
        };
        let router = match &self.fork_scope {
            Some(scope) => super::module_host::with_lifecycle(scope.lifecycle.clone(), || {
                super::module_host::with_tool_registry(scope.registry.clone(), || {
                    match &scope.live {
                        Some(live) => {
                            super::module_host::with_live_state(live.clone(), build_router)
                        }
                        None => build_router(),
                    }
                })
            }),
            None => super::module_host::with_lifecycle(self.lifecycle.clone(), || {
                super::module_host::with_file_update_hook(
                    self.file_changed_lifecycle.as_ref().map(|lifecycle| {
                        crate::runtime::hooks::session::FileUpdateHook {
                            lifecycle: lifecycle.clone(),
                            session_key: self.config.session_key.clone(),
                            cwd: self.config.cwd.clone(),
                        }
                    }),
                    build_router,
                )
            }),
        }
        .map_err(|error| RuntimeError::new(error.to_string()))?;
        let mut execute_input = input.as_object().cloned().unwrap_or_default();
        execute_input.insert(
            "hostModules".into(),
            super::module_host::host_manifest(&router),
        );
        let request =
            build_execute_request(&self.config, &JsValue::Object(execute_input), &options)
                .map_err(RuntimeError::new)?;
        let handler = Arc::new(ModuleHost {
            live,
            router,
            events,
            steers: steers.clone(),
            persistence,
            transcript: self.transcript.clone(),
            cancellation: cancellation.clone(),
            run_id: options
                .get("execution")
                .and_then(|v| v.get("runId"))
                .and_then(JsValue::as_string)
                .cloned(),
            session_id: self.config.session_key.clone(),
            turn_id: turn_id.clone(),
        });
        {
            let mut active = self.active.lock().unwrap();
            if active.is_some() || self.disposed.load(Ordering::SeqCst) {
                return Err(RuntimeError::new(
                    "Agent session already has an active turn or is disposed.",
                ));
            }
            *active = Some(ActiveTurn {
                accepting: true,
                identity: identity.clone(),
                steers: steers.clone(),
                cancellation: cancellation.clone(),
                host_cancellation: cancellation_for_host,
                completion: completion.clone(),
                sink: sink.clone(),
            });
        }
        let limits = self.config.limits;
        let worker = handler.clone();
        let pump = sink.clone();
        let active = self.active.clone();
        let embedded_worker = self.embedded_worker.clone();
        let spawner = self.spawner.clone();
        let accepted_messages = input
            .get("acceptedMessages")
            .and_then(|messages| match messages {
                JsValue::Array(messages) => Some(messages.clone()),
                _ => None,
            })
            .unwrap_or_else(|| messages_of(&input).last().cloned().into_iter().collect());
        let accepted_metadata = crate::runtime::turn::submit_host::accepted_input_metadata(&input);
        let fork_scope = self.fork_scope.is_some();
        let hook_subscription = if fork_scope {
            Vec::new()
        } else {
            let mut subscriptions = Vec::new();
            for lifecycle in std::iter::once(&self.lifecycle)
                .chain(self.config_change_lifecycle.iter())
                .chain(self.file_changed_lifecycle.iter())
            {
                let event_sink = sink.clone();
                let session_id = self.config.session_key.clone();
                let native_turn_id = turn_id.clone();
                subscriptions.push(
                    lifecycle
                        .subscribe_hook_execution_events(Arc::new(
                            crate::runtime::hooks::GatewayHookExecutionProjection::new(
                                session_id.clone(),
                                if self.include_hook_events {
                                    crate::runtime::hooks::HookEventProjection::Sdk
                                } else {
                                    crate::runtime::hooks::HookEventProjection::Gateway
                                },
                                move |mut event| {
                                    event.insert(
                                        "sessionId".into(),
                                        JsValue::String(session_id.clone()),
                                    );
                                    event.insert(
                                        "turnId".into(),
                                        JsValue::String(native_turn_id.clone()),
                                    );
                                    event_sink.send(Some(Ok(JsValue::Object(event)))).is_ok()
                                },
                            ),
                        ))
                        .map_err(|error| {
                            self.active.lock().unwrap().take();
                            RuntimeError::new(error.to_string())
                        })?,
                );
            }
            subscriptions
        };
        let hook_binding = self.async_hook_bindings.as_ref().map(|bindings| {
            let event_sink = sink.clone();
            let run_id = options
                .get("execution")
                .and_then(|execution| execution.get("runId"))
                .and_then(JsValue::as_string)
                .cloned()
                .unwrap_or_else(|| turn_id.clone());
            bindings.bind(
                self.config.session_key.clone(),
                crate::runtime::async_hooks::AsyncHookTurn {
                    run_id,
                    mailbox: steers.mailbox.clone(),
                },
                move |event| {
                    let _ = event_sink.send(Some(Ok(JsValue::Object(event))));
                },
            )
        });
        let subagent_providers = self.subagent_providers.clone();
        tokio::spawn(async move {
            let _subagent_providers =
                super::module_host::SubagentProvidersScope::enter(subagent_providers);
            let _guard = ActiveGuard {
                active,
                identity,
                sink: pump.clone(),
                steers,
                turn_id,
                completed,
                hook_binding,
                hook_subscription,
            };
            if let Some(transcript) = &worker.transcript {
                if let Err(error) = transcript.record_accepted_input(
                    &worker.session_id,
                    &worker.turn_id,
                    accepted_messages,
                    accepted_metadata,
                ) {
                    let _ = pump.send(Some(Err(RuntimeError::new(error.to_string()))));
                    return;
                }
                let mut started = JsObject::new();
                started.insert("type".into(), "turn_started".into());
                if let Err(error) = transcript.record_native_session_event(
                    &worker.session_id,
                    &worker.turn_id,
                    started,
                ) {
                    let _ = pump.send(Some(Err(RuntimeError::new(error.to_string()))));
                    return;
                }
                let mut accepted = JsObject::new();
                let _ = pump.send(Some(Ok(crate::metadata::object([
                    ("type", "turn_started".into()),
                    ("sessionId", JsValue::String(worker.session_id.clone())),
                    ("turnId", JsValue::String(worker.turn_id.clone())),
                ]))));
                accepted.insert("type".into(), "input_accepted".into());
                accepted.insert(
                    "sessionId".into(),
                    JsValue::String(worker.session_id.clone()),
                );
                accepted.insert("turnId".into(), JsValue::String(worker.turn_id.clone()));
                let _ = pump.send(Some(Ok(JsValue::Object(accepted))));
            }
            let result = if external {
                match spawner.connect(limits).await {
                    Ok(Some(client)) => {
                        client
                            .execute_cancellable(request, worker.clone(), cancellation.clone())
                            .await
                    }
                    Ok(None) => {
                        embedded_worker
                            .execute(request, worker.clone(), limits, cancellation.clone())
                            .await
                    }
                    Err(error) => {
                        let _ = worker.close_steers().await;
                        let _ = pump.send(Some(Err(RuntimeError::new(error.to_string()))));
                        return;
                    }
                }
            } else {
                embedded_worker
                    .execute(request, worker.clone(), limits, cancellation.clone())
                    .await
            };
            if let Err(error) = worker.close_steers().await {
                let _ = pump.send(Some(Err(RuntimeError::new(error.to_string()))));
                return;
            }
            match result {
                Ok(ExecuteResult::Terminal(payload)) => {
                    // The terminal frame the session layer expects last.
                    let terminal = worker
                        .finish_terminal(payload)
                        .map_err(ModuleHost::runtime_failure);
                    let _ = pump.send(Some(terminal));
                }
                Ok(ExecuteResult::Aborted { terminal }) => {
                    {
                        let terminal = worker
                            .persist_terminal(
                                terminal
                                    .unwrap_or_else(|| worker.transport_terminal("aborted", None)),
                            )
                            .map_err(|error| RuntimeError::new(error.to_string()));
                        let _ = pump.send(Some(terminal));
                    }
                    let _ = pump.send(Some(Ok(crate::metadata::object([
                        ("type", "session_aborted".into()),
                        ("sessionId", JsValue::String(worker.session_id.clone())),
                        ("turnId", JsValue::String(worker.turn_id.clone())),
                        ("reason", "agent_aborted".into()),
                    ]))));
                }
                Ok(ExecuteResult::Rejected(payload)) => {
                    let terminal = worker
                        .finish_rejected(payload, fork_scope)
                        .map_err(|error| RuntimeError::new(error.to_string()));
                    let _ = pump.send(Some(terminal));
                }
                Err(error) => {
                    if (error.kind != crate::host::HostErrorKind::ResultUnknown
                        || !worker.timed_out_unknown())
                        && let Err(error) =
                            worker.persist_terminal(worker.transport_terminal(
                                "error",
                                Some(error.to_string().as_str().into()),
                            ))
                    {
                        let _ = pump.send(Some(Err(RuntimeError::new(error.to_string()))));
                    }
                    let _ = pump.send(Some(Err(ModuleHost::runtime_failure(error))));
                }
            }
        });
        Ok(Box::new(SidecarTurn {
            receiver,
            completion,
            ended: false,
        }))
    }

    fn steer(&self, turn_id: JsString, message: SteerMessage) -> HostFuture<EnqueueResult> {
        let mailbox = self
            .active
            .lock()
            .unwrap()
            .as_ref()
            .filter(|turn| turn.accepting)
            .map(|turn| turn.steers.mailbox.clone());
        if self.disposed.load(Ordering::SeqCst) || mailbox.is_none() {
            return Box::pin(async {
                Ok(EnqueueResult {
                    accepted: false,
                    reason: Some(SteerReason::NoActiveTurn),
                })
            });
        }
        mailbox.unwrap().enqueue(turn_id, message)
    }

    fn cancel_steer(&self, turn_id: JsString, item_id: JsString) -> HostFuture<CancelResult> {
        let mailbox = self
            .active
            .lock()
            .unwrap()
            .as_ref()
            .filter(|turn| turn.accepting)
            .map(|turn| turn.steers.mailbox.clone());
        if self.disposed.load(Ordering::SeqCst) || mailbox.is_none() {
            return Box::pin(async {
                Ok(CancelResult {
                    cancelled: false,
                    reason: Some(SteerReason::NoActiveTurn),
                })
            });
        }
        mailbox.unwrap().cancel(turn_id, item_id)
    }

    fn snapshot(&self) -> JsValue {
        let mut object = JsObject::new();
        object.insert(
            "sessionKey".into(),
            JsValue::String(self.config.session_key.clone()),
        );
        if let Some(project) = &self.config.project_key {
            object.insert("projectKey".into(), JsValue::String(project.clone()));
        }
        object.insert("cwd".into(), JsValue::String(self.config.cwd.clone()));
        object.insert(
            "active".into(),
            JsValue::Bool(self.active.lock().unwrap().is_some()),
        );
        JsValue::Object(object)
    }

    fn abort(&self, reason: &JsString) -> RuntimeResult {
        if self.disposed.load(Ordering::SeqCst) {
            return Ok(());
        }
        if let Some(turn) = self.active.lock().unwrap().as_mut() {
            turn.accepting = false;
            turn.cancellation.cancel(Some(reason.clone()));
            turn.host_cancellation.cancel_synchronously();
        }
        Ok(())
    }

    fn dispose(&self) -> DisposeFuture<'_> {
        self.disposed.store(true, Ordering::SeqCst);
        let active = self.active.lock().unwrap().take();
        let mcp_owner = self.mcp_owner.clone().filter(|_| self.fork_scope.is_none());
        let context_resources = self
            .owns_context_resources
            .then(|| self.context_resources.clone());
        let router_state = self.router_state.clone();
        let lifecycle = self.lifecycle.clone();
        let plugin_lease = &self.plugin_lease;
        let owns_lifecycle = self.fork_scope.is_none();
        let config_lifecycle = self.config_change_lifecycle.clone();
        let file_lifecycle = self.file_changed_lifecycle.clone();
        let sdk_handlers = &self.sdk_handlers;
        let embedded_worker = self.embedded_worker.clone();
        Box::pin(async move {
            if let Some(turn) = active {
                turn.host_cancellation.cancel_synchronously();
                turn.cancellation.cancel(Some("session_disposed".into()));
                turn.host_cancellation.cancel();
                let _ = turn.sink.send(None);
                let mut completion = turn.completion;
                let _ = completion.wait_for(|done| *done).await;
            }
            embedded_worker.shutdown().await;
            if let Some(owner) = mcp_owner {
                owner.dispose().await?;
            }
            if let Some(resources) = context_resources {
                resources.dispose_owner().await.map_err(RuntimeError::new)?;
            }
            router_state.dispose();
            if owns_lifecycle {
                lifecycle.dispose().await;
            }
            if let Some(runtime) = config_lifecycle {
                runtime.dispose().await;
            }
            if let Some(runtime) = file_lifecycle {
                runtime.dispose().await;
            }
            if let Some(handlers) = sdk_handlers.lock().await.take() {
                let mut registry = handlers.registry.lock().await;
                if let Some(config) = handlers.config {
                    registry.handlers_mut().unregister_config_change(&config);
                }
                registry
                    .handlers_mut()
                    .unregister_session_hook(&handlers.session);
            }
            if let Some(mut lease) = plugin_lease.lock().await.take() {
                lease
                    .release()
                    .await
                    .map_err(|error| RuntimeError::new(error.message))?;
            }
            Ok(())
        })
    }
}

impl SidecarSession {
    #[cfg(test)]
    fn install_sink(
        &self,
        sink: mpsc::UnboundedSender<Option<Result<JsValue, RuntimeError>>>,
        cancellation: Cancellation,
    ) {
        let turn_id = JsString::from("test-turn");
        let mailbox = SteerMailbox::new(Some(Arc::new(SteerRecorder(self.persistence.clone()))));
        mailbox.start(turn_id.clone());
        *self.active.lock().unwrap() = Some(ActiveTurn {
            accepting: true,
            identity: Arc::new(()),
            steers: TurnSteers::new(mailbox, turn_id.clone(), Vec::new()),
            cancellation,
            sink,
            host_cancellation: crate::runtime::network::Cancellation::default(),
            // This fixture has no spawned worker to drain.
            completion: watch::channel(true).1,
        });
    }
}

pub struct SidecarTurn {
    receiver: mpsc::UnboundedReceiver<Option<Result<JsValue, RuntimeError>>>,
    ended: bool,
    completion: watch::Receiver<bool>,
}

impl LiveTurn for SidecarTurn {
    fn next(&mut self) -> SessionNext<'_> {
        Box::pin(async move {
            if self.ended {
                return Ok(None);
            }
            match self.receiver.recv().await {
                Some(Some(Ok(event))) => Ok(Some(event)),
                Some(Some(Err(error))) => {
                    let _ = self.completion.wait_for(|done| *done).await;
                    self.ended = true;
                    Err(error)
                }
                Some(None) | None => {
                    let _ = self.completion.wait_for(|done| *done).await;
                    self.ended = true;
                    Ok(None)
                }
            }
        })
    }
}

/// The production module router plus the turn's event buffer. The event
/// adapter owns delivery; `module_call` runs the real host modules.
struct ModuleHost {
    live: Option<Arc<super::routes::live::LiveSessionState>>,
    router: crate::host::modules::ModuleRouter,
    events: Arc<super::module_host::EventBuffer>,
    steers: TurnSteers,
    persistence: Option<Arc<dyn SteerPersistence>>,
    transcript: Option<Arc<TranscriptWriter>>,
    cancellation: Cancellation,
    run_id: Option<JsString>,
    session_id: JsString,
    turn_id: JsString,
}

impl HostHandler for ModuleHost {
    fn module_call(&self, call: JsValue) -> HostFuture<JsObject> {
        if call
            .get("module")
            .and_then(JsValue::as_string)
            .is_some_and(|module| module.equals_str("event"))
            && call
                .get("payload")
                .and_then(|payload| payload.get("operation"))
                .and_then(JsValue::as_string)
                .is_some_and(|operation| operation.equals_str("emit"))
        {
            let event = call
                .get("payload")
                .and_then(|payload| payload.get("event"))
                .cloned()
                .unwrap_or(JsValue::Undefined);
            let delivered = self.deliver_event(event);
            return Box::pin(async move {
                delivered.await?;
                let mut response = JsObject::new();
                response.insert("result".into(), JsValue::Null);
                Ok(response)
            });
        }
        self.router.module_call(call)
    }
    fn event(&self, event: JsValue) -> HostFuture<()> {
        // Module transport frames wrap the AgentEvent; final is delivered by
        // execute's terminal result, so it must not be emitted a second time.
        if event.get("final").and_then(JsValue::as_bool) == Some(true) {
            return Box::pin(async { Ok(()) });
        }
        let payload = event.get("payload").cloned().unwrap_or(event);
        self.deliver_event(payload)
    }
    fn dispose(&self) -> HostFuture<()> {
        self.router.dispose()
    }
}

impl ModuleHost {
    fn timed_out_unknown(&self) -> bool {
        let Some(run) = &self.run_id else {
            return false;
        };
        let mut reason = "timeout:".encode_utf16().collect::<Vec<_>>();
        reason.extend_from_slice(run.units());
        self.cancellation
            .reason()
            .as_ref()
            .is_some_and(|actual| actual.units() == reason)
    }
    fn runtime_failure(error: HostError) -> RuntimeError {
        let mut failure = RuntimeError::new(error.to_string());
        if error.kind == crate::host::HostErrorKind::ResultUnknown
            && let Some(canonical) = error.module_error.and_then(|error| error.canonical)
        {
            // RuntimeError has no wire-value slot. Preserve the immutable
            // protocol envelope losslessly in its diagnostic cause instead
            // of manufacturing an AgentTurnResult from an unknown operation.
            if let Ok(Some(payload)) = stringify_json(&JsValue::Object(canonical)) {
                failure.causes.push(RuntimeError::new(payload));
            }
        }
        failure
    }
    fn finish_terminal(&self, payload: JsValue) -> Result<JsValue, HostError> {
        if payload
            .get("outcome")
            .and_then(JsValue::as_string)
            .is_some_and(|outcome| outcome.equals_str("result_unknown"))
        {
            if !self.timed_out_unknown() {
                // TurnRunner:420-428 only rethrows timeout:<runId>. Other
                // unknown outcomes become agent_invalid_state error results.
                let now = crate::runtime::context::prompt::iso_timestamp(
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_millis() as f64,
                );
                return self.persist_terminal(crate::metadata::object([("result", crate::metadata::object([
                    ("type", "error".into()), ("sessionId", JsValue::String(self.session_id.clone())),
                    ("turnId", JsValue::String(self.turn_id.clone())), ("stopReason", "model_error".into()),
                    ("usage", crate::metadata::object([])), ("permissionDenials", JsValue::Array(vec![])),
                    ("turns", JsValue::Number(0.0)), ("startedAt", now.as_str().into()), ("completedAt", now.as_str().into()),
                    ("errors", JsValue::Array(vec![crate::metadata::object([
                        ("code", "agent_invalid_state".into()),
                        ("message", "AgentLoop terminal outcome is result_unknown and host reconciliation found no final result.".into()),
                    ])])),
                ]))]));
            }
            // TS client:663-665,1017-1034 requires host reconciliation before
            // publishing a result. TurnRunner:420-424,701-704 deliberately
            // leaves timed-out unknown operations without a turn_result.
            let mut error = HostError::new(
                crate::host::HostErrorKind::ResultUnknown,
                "Sidecar terminal outcome is result_unknown; host reconciliation is required.",
            );
            error.accepted = true;
            error.stream_id = payload
                .get("streamId")
                .and_then(JsValue::as_string)
                .cloned();
            let mut detail = ModuleError::new(error.message.as_str());
            detail.code = Some("AGENT_LOOP_RESULT_UNKNOWN".into());
            detail.canonical = payload.as_object().cloned();
            error.module_error = Some(Box::new(detail));
            return Err(error);
        }
        self.persist_terminal(payload)
    }
    fn finish_rejected(&self, payload: JsValue, fork_scope: bool) -> Result<JsValue, HostError> {
        if !fork_scope
            && self.transcript.is_none()
            && self.live.is_none()
            && payload
                .get("payload")
                .unwrap_or(&payload)
                .get("result")
                .is_none()
        {
            // A bare transport adapter has no AgentSession recorder. Preserve
            // its Module response contract; production sessions below project
            // and persist exactly one AgentEvent terminal (TS client:642-651).
            return Ok(payload);
        }
        self.persist_terminal(self.rejected_terminal(payload))
    }
    fn rejected_terminal(&self, payload: JsValue) -> JsValue {
        if payload
            .get("payload")
            .unwrap_or(&payload)
            .get("result")
            .is_some()
        {
            return payload;
        }
        // Cancellation events without a result use the native abort contract.
        // Failed execute responses follow TS rejectedExecuteTerminal:1359-1388.
        let cancelled = payload
            .get("outcome")
            .and_then(JsValue::as_string)
            .is_some_and(|outcome| outcome.equals_str("cancelled"));
        if cancelled {
            return self.transport_terminal("aborted", Some(payload));
        }
        let error = payload.get("error").and_then(JsValue::as_object);
        let code = payload
            .get("code")
            .and_then(JsValue::as_string)
            .or_else(|| {
                error
                    .and_then(|error| error.get("code"))
                    .and_then(JsValue::as_string)
            });
        let message = error
            .and_then(|error| error.get("message"))
            .and_then(JsValue::as_string)
            .cloned()
            .unwrap_or_else(|| match code.filter(|code| !code.is_empty()) {
                Some(code) => {
                    let mut units = "Sidecar rejected execute request: "
                        .encode_utf16()
                        .collect::<Vec<_>>();
                    units.extend_from_slice(code.units());
                    JsString::from_utf16(units)
                }
                None => "Sidecar rejected execute request.".into(),
            });
        let mut details = JsObject::new();
        if let Some(code) = code.filter(|code| !code.is_empty()) {
            details.insert("sidecarCode".into(), JsValue::String(code.clone()));
        }
        if let Some(error) = payload.get("error").filter(|error| !error.is_undefined()) {
            details.insert("sidecarError".into(), error.clone());
        }
        let epoch_ms = match std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) {
            Ok(duration) => duration.as_millis() as i64,
            Err(error) => -(error.duration().as_millis() as i64),
        };
        let now = pilotdeck_transport::system_clock::format_epoch_iso(epoch_ms);
        crate::metadata::object([(
            "result",
            crate::metadata::object([
                ("type", "error".into()),
                ("sessionId", JsValue::String(self.session_id.clone())),
                ("turnId", JsValue::String(self.turn_id.clone())),
                (
                    "stopReason",
                    if code.is_some_and(|code| code.equals_str("DEADLINE_EXCEEDED")) {
                        "aborted_streaming".into()
                    } else {
                        "model_error".into()
                    },
                ),
                ("usage", crate::metadata::object([])),
                ("permissionDenials", JsValue::Array(vec![])),
                ("turns", JsValue::Number(0.0)),
                ("startedAt", now.as_str().into()),
                ("completedAt", now.as_str().into()),
                (
                    "errors",
                    JsValue::Array(vec![crate::metadata::object([
                        ("code", "agent_execution_rejected".into()),
                        ("message", JsValue::String(message)),
                        ("details", JsValue::Object(details)),
                    ])]),
                ),
            ]),
        )])
    }
    fn transport_terminal(&self, kind: &str, error: Option<JsValue>) -> JsValue {
        let mut result = crate::metadata::object([
            ("sessionId", JsValue::String(self.session_id.clone())),
            ("turnId", JsValue::String(self.turn_id.clone())),
            ("type", kind.into()),
            ("stopReason", kind.into()),
            ("usage", crate::metadata::object([])),
            ("permissionDenials", JsValue::Array(vec![])),
        ]);
        if let (Some(error), JsValue::Object(fields)) = (error, &mut result) {
            fields.insert("error".into(), error);
        }
        crate::metadata::object([("result", result)])
    }
    fn deliver_event(&self, event: JsValue) -> HostFuture<()> {
        use crate::host::modules::event::EventSink;
        let events = self.events.clone();
        let steers = self.steers.clone();
        let persistence = self.persistence.clone();
        let transcript = self.transcript.clone();
        let handler = self.router.turn.clone();
        let session_id = self.session_id.clone();
        let turn_id = self.turn_id.clone();
        Box::pin(async move {
            let fields = event
                .as_object()
                .ok_or_else(|| ModuleError::new("Invalid sidecar event."))?;
            if fields.get("sessionId").and_then(JsValue::as_string) != Some(&session_id)
                || fields
                    .get("turnId")
                    .is_some_and(|id| id.as_string() != Some(&turn_id))
                || fields.get("type").and_then(JsValue::as_string).is_none()
            {
                return Err(
                    ModuleError::new("Sidecar host event does not match the active turn.").into(),
                );
            }
            if fields
                .get("type")
                .and_then(JsValue::as_string)
                .is_some_and(|kind| kind.equals_str("steer_applied"))
            {
                let item_id = fields
                    .get("itemId")
                    .and_then(JsValue::as_string)
                    .cloned()
                    .ok_or_else(|| ModuleError::new("Invalid applied steer itemId."))?;
                let message = fields
                    .get("message")
                    .cloned()
                    .ok_or_else(|| ModuleError::new("Invalid applied steer message."))?;
                if !steers
                    .unacknowledged()
                    .iter()
                    .any(|steer| steer.item_id == item_id)
                    || message
                        .get("metadata")
                        .and_then(|metadata| metadata.get("queueItemId"))
                        .and_then(JsValue::as_string)
                        != Some(&item_id)
                {
                    return Err(
                        ModuleError::new("Applied steer was not offered by this turn.").into(),
                    );
                }
                let persistence = persistence.ok_or_else(|| {
                    ModuleError::new("The active session has no transcript writer.")
                })?;
                handler
                    .persist_and_apply_steer(steers, item_id, message, move |message| {
                        persistence.message(turn_id, message)
                    })
                    .await?;
            } else if let Some(transcript) = transcript {
                let kind = fields.get("type").and_then(JsValue::as_string).unwrap();
                let durable_kind = if kind.equals_str("assistant_message") {
                    Some("assistant_message")
                } else if kind.equals_str("tool_results_projected") {
                    Some("tool_result_message")
                } else {
                    None
                };
                if let Some(kind) = durable_kind {
                    let message = fields
                        .get("message")
                        .cloned()
                        .ok_or_else(|| ModuleError::new("A durable sidecar message is missing."))?;
                    let mut draft = JsObject::new();
                    draft.insert("type".into(), kind.into());
                    draft.insert("message".into(), message);
                    transcript
                        .record_native_session_event(&session_id, &turn_id, draft)
                        .map_err(|error| ModuleError::new(error.to_string()))?;
                }
            }
            events.emit(fields.clone())
        })
    }

    fn persist_terminal(&self, terminal: JsValue) -> Result<JsValue, HostError> {
        let payload = terminal.get("payload").unwrap_or(&terminal);
        let result = payload
            .get("result")
            .cloned()
            .ok_or_else(|| ModuleError::new("Native sidecar terminal has no result."))?;
        if let Some(transcript) = &self.transcript {
            let mut draft = JsObject::new();
            draft.insert("type".into(), "turn_result".into());
            draft.insert("result".into(), result.clone());
            transcript
                .record_native_session_event(&self.session_id, &self.turn_id, draft)
                .map_err(|error| ModuleError::new(error.to_string()))?;
        } else if let Some(live) = &self.live {
            // Commit the real terminal before a timeout can drop its live consumer.
            live.complete_turn(&result)
                .map_err(|error| ModuleError::new(error.to_string()))?;
        }
        let mut event = JsObject::new();
        event.insert("type".into(), "turn_completed".into());
        event.insert("sessionId".into(), JsValue::String(self.session_id.clone()));
        event.insert("turnId".into(), JsValue::String(self.turn_id.clone()));
        event.insert("result".into(), result);
        Ok(JsValue::Object(event))
    }

    async fn close_steers(&self) -> Result<(), HostError> {
        use crate::host::modules::event::EventSink;
        let mut unapplied = self.steers.unacknowledged();
        for steer in self.steers.mailbox.close(self.turn_id.clone()).await? {
            if !unapplied
                .iter()
                .any(|message| message.item_id == steer.item_id)
            {
                unapplied.push(steer);
            }
        }
        for steer in unapplied {
            let mut event = JsObject::new();
            event.insert("type".into(), "steer_unapplied".into());
            event.insert("sessionId".into(), JsValue::String(self.session_id.clone()));
            event.insert("turnId".into(), JsValue::String(self.turn_id.clone()));
            event.insert("itemId".into(), JsValue::String(steer.item_id));
            event.insert("message".into(), steer.message);
            event.insert("reason".into(), "turn_ended".into());
            self.events.emit(event)?;
        }
        Ok(())
    }
}

fn build_execute_request(
    config: &SidecarSessionConfig,
    input: &JsValue,
    options: &JsValue,
) -> Result<ExecuteRequest, String> {
    let run_id = string_of(options, "turnId").unwrap_or_default();
    let identity = pilotdeck_protocol::admission::ExecutionIdentity {
        run_id: run_id.clone(),
        operation_id: string_of(options, "operationId").unwrap_or_else(|| run_id.clone()),
        request_id: format!("execute-{}", uuid::Uuid::new_v4()).into(),
        session_id: config.session_key.clone(),
        turn_id: run_id.clone(),
    };
    let request_id = identity.request_id.clone();
    let operation_id = identity.operation_id.clone();
    let session_id = identity.session_id.clone();
    let turn_id = identity.turn_id.clone();
    let run_id_value = identity.run_id.clone();
    let agent = input.get("agent").cloned().unwrap_or_else(|| {
        let mut agent = JsObject::new();
        agent.insert("cwd".into(), JsValue::String(config.cwd.clone()));
        JsValue::Object(agent)
    });
    let base_permission_mode =
        string_of(input, "basePermissionMode").and_then(|value| value.to_utf8().ok());
    let encoded = encode_execute(&ExecutePayloadInput {
        run_id: &run_id_value,
        operation_id: &operation_id,
        request_id: &request_id,
        session_id: &session_id,
        turn_id: &turn_id,
        agent,
        messages: messages_of(input),
        max_turns: number_of(input, "maxTurns"),
        max_budget_usd: number_of(input, "maxBudgetUsd"),
        task_budget_usd: number_of(input, "taskBudgetUsd"),
        initial_task_budget_spent_usd: number_of(input, "initialTaskBudgetSpentUsd"),
        can_elicit: input.get("canElicit").and_then(JsValue::as_bool),
        model_override: input.get("modelOverride").cloned(),
        base_permission_mode: base_permission_mode.as_deref(),
        allow_plan_mode_tools: input.get("allowPlanModeTools").and_then(JsValue::as_bool),
        tools: match input.get("tools") {
            Some(JsValue::Array(items)) => items.clone(),
            _ => vec![],
        },
        permission_context: input
            .get("permissionContext")
            .cloned()
            .unwrap_or(JsValue::Null),
        host_modules: input.get("hostModules").cloned().unwrap_or(JsValue::Null),
        interaction_capabilities: input
            .get("interactionCapabilities")
            .cloned()
            .unwrap_or(JsValue::Null),
        seed_state: input.get("seedState").cloned(),
        model_state: input.get("modelState").cloned(),
    });
    let body = encoded
        .get("payload")
        .and_then(JsValue::as_object)
        .cloned()
        .ok_or_else(|| "Execute encoder did not return an object payload.".to_string())?;
    Ok(ExecuteRequest {
        identity: pilotdeck_protocol::admission::ExecutionIdentity {
            run_id: run_id_value,
            operation_id,
            request_id,
            session_id,
            turn_id,
        },
        payload: body,
        idempotency_key: None,
        operation_deadline: options
            .get("execution")
            .and_then(|e| e.get("operationDeadline"))
            .and_then(JsValue::as_string)
            .cloned(),
    })
}

fn messages_of(input: &JsValue) -> Vec<JsValue> {
    match input.get("messages") {
        Some(JsValue::Array(items)) => items.clone(),
        _ => vec![],
    }
}

fn string_of(value: &JsValue, key: &str) -> Option<JsString> {
    value.get(key).and_then(JsValue::as_string).cloned()
}

fn number_of(value: &JsValue, key: &str) -> Option<f64> {
    value.get(key).and_then(JsValue::as_number)
}

#[cfg(test)]
mod tests {
    use super::*;
    use pilotdeck_protocol::parse_json;

    fn session() -> (
        Arc<SidecarSession>,
        mpsc::UnboundedReceiver<Option<Result<JsValue, RuntimeError>>>,
    ) {
        struct NoSpawn;
        impl SidecarSpawner for NoSpawn {
            fn requires_subprocess(&self) -> bool {
                true
            }

            fn command(&self) -> std::io::Result<tokio::process::Command> {
                Ok(tokio::process::Command::new("true"))
            }
        }
        let session = Arc::new(SidecarSession::new(
            SidecarSessionConfig {
                limits: Limits::default(),
                session_key: "s".into(),
                project_key: None,
                cwd: "/tmp".into(),
                snapshot: JsValue::Null,
                services: None,
            },
            Arc::new(NoSpawn),
        ));
        let (sink, receiver) = mpsc::unbounded_channel();
        session.install_sink(sink.clone(), Cancellation::default());
        let _ = sink; // keep the sender alive for the test
        (session, receiver)
    }

    /// handle.rs:39 — an abort must settle the active turn instead of leaving
    /// a consumer pending forever.
    #[test]
    fn abort_cancels_provider_and_driver_without_truncating_durable_tail() {
        let (_session, mut receiver) = session();
        let session = _session;
        session.abort(&"user_stop".into()).unwrap();
        let active = session.active.lock().unwrap();
        let active = active.as_ref().expect("driver still owns cleanup");
        assert!(active.cancellation.is_cancelled());
        assert!(active.host_cancellation.is_cancelled());
        assert!(
            receiver.try_recv().is_err(),
            "driver must drain its terminal before EOF"
        );
    }

    /// Disposal is idempotent and suppresses later aborts (handle.rs:43).
    #[tokio::test]
    async fn dispose_is_idempotent_and_suppresses_abort() {
        let (session, _receiver) = session();
        session.dispose().await.unwrap();
        session.dispose().await.unwrap();
        assert!(session.abort(&"late".into()).is_ok());
    }

    /// The encoder must carry the turn identity the sidecar validates against.
    #[tokio::test]
    async fn submit_persists_metadata_and_emits_started_before_accepted() {
        use crate::storage::{AsyncTranscriptAdapter, JsonlTranscriptStore, SessionStorageInput};
        struct NoSpawn;
        impl SidecarSpawner for NoSpawn {
            fn requires_subprocess(&self) -> bool {
                true
            }

            fn command(&self) -> std::io::Result<tokio::process::Command> {
                Ok(tokio::process::Command::new("false"))
            }
        }
        let root =
            std::env::temp_dir().join(format!("pdrs-session-order-{}", uuid::Uuid::new_v4()));
        let chat = root.join("chat");
        let adapter = AsyncTranscriptAdapter::new(
            Arc::new(JsonlTranscriptStore::new()),
            Arc::new(move |_| chat.clone()),
        );
        let storage = adapter.create_session_storage(SessionStorageInput {
            project_root: root.clone(),
            pilot_home: root.clone(),
            session_id: "s".into(),
            now: None,
        })
        .unwrap();
        let writer = Arc::new(storage.transcript);
        let session = SidecarSession::with_transcript(
            SidecarSessionConfig {
                limits: Limits::default(),
                session_key: "s".into(),
                project_key: None,
                cwd: root.to_string_lossy().as_ref().into(),
                snapshot: JsValue::Null,
                services: None,
            },
            Arc::new(NoSpawn),
            writer.clone(),
        );
        let mut turn = session.submit(
            parse_json(r#"{"agent":{},"runMode":"plan","allowPlanModeTools":false,"messages":[{"role":"user","content":[{"type":"text","text":"hi"}]}]}"#).unwrap(),
            parse_json(r#"{"turnId":"t","operationId":"op"}"#).unwrap(),
        ).unwrap();
        for kind in ["turn_started", "input_accepted"] {
            let event = tokio::time::timeout(std::time::Duration::from_secs(5), turn.next())
                .await
                .unwrap()
                .unwrap()
                .unwrap();
            assert_eq!(event.get("type"), Some(&JsValue::from(kind)));
            assert_eq!(event.get("sessionId"), Some(&JsValue::from("s")));
            assert_eq!(event.get("turnId"), Some(&JsValue::from("t")));
        }
        let entries = crate::storage::read_transcript(writer.path())
            .unwrap()
            .entries;
        let accepted = entries
            .iter()
            .find(|e| e.get("type") == Some(&"accepted_input".into()))
            .unwrap();
        assert_eq!(
            accepted.get("metadata").and_then(|m| m.get("runMode")),
            Some(&"plan".into())
        );
        assert_eq!(
            accepted
                .get("metadata")
                .and_then(|m| m.get("allowPlanModeTools")),
            Some(&JsValue::Bool(false))
        );
        assert!(
            entries
                .iter()
                .any(|e| e.get("type") == Some(&"turn_started".into()))
        );
        while let Ok(Some(_)) = turn.next().await {}
        session.dispose().await.unwrap();
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn execute_request_carries_identity_and_the_unwrapped_body() {
        let config = SidecarSessionConfig {
            limits: Limits::default(),
            session_key: "session-1".into(),
            project_key: None,
            cwd: "/tmp".into(),
            snapshot: JsValue::Null,
            services: None,
        };
        let options = parse_json(r#"{"turnId":"turn-1","operationId":"op-1"}"#).unwrap();
        let input = parse_json(
            r#"{"agent":{"provider":"p","model":"m"},"messages":[{"role":"user","content":"hi"}]}"#,
        )
        .unwrap();
        let request = build_execute_request(&config, &input, &options).unwrap();
        assert_eq!(
            request.identity.run_id.to_utf8().ok().as_deref(),
            Some("turn-1")
        );
        assert_eq!(
            request.identity.session_id.to_utf8().ok().as_deref(),
            Some("session-1")
        );
        assert!(request.payload.get("kind").is_none());
        assert!(request.payload.get("payload").is_none());
        assert!(request.payload.get("agent").is_some());
        assert!(request.payload.get("messages").is_some());
        assert!(request.payload.get("hostModules").is_some());
        assert!(request.payload.get("agent").is_some());
        assert!(request.payload.get("messages").is_some());
        assert!(
            request.payload.get("payload").is_none(),
            "HostClient owns the single wire envelope"
        );
        let envelope = crate::metadata::object([
            ("sessionId", JsValue::String(request.identity.session_id)),
            ("turnId", JsValue::String(request.identity.turn_id)),
            ("payload", JsValue::Object(request.payload)),
        ]);
        use pilotdeck_transport::execute::FactoryPayloadDecoder;
        let decoded = pilotdeck_transport::execute::OriginalFactoryPayloadDecoder
            .decode(&envelope, &"/tmp".into());
        assert!(decoded.is_ok());
    }
}

#[cfg(test)]
#[path = "sidecar_steer_tests.rs"]
mod steer_tests;
