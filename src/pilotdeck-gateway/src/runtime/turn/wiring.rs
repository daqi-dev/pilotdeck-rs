//! Production gateway wiring: one runtime owner behind the WS pairing layer.
//!
//! Composes the native providers into a TurnGateway and routes the local
//! methods through it instead of the TS upstream. submit_turn therefore runs on
//! the Rust runtime; a method with no native owner returns an explicit error so
//! a half-wired gateway can never look successful.
use super::coordinator::TurnEventCoordinator;
use super::gateway::{GatewayRequests, GatewayServices, TurnGateway};
use super::projector::GatewayAgentEventProjector;
use super::publish::FileRegistry;
use super::read;
use super::replay::TurnReplayOptions;
use super::submit::{TurnSubmitter, WorkspaceAdmissions};
use super::submit_host::{NativeSubmitHost, NativeSubmitProviders};
use crate::metadata::object;
use crate::runtime::connection::{Call, RequestError};
use crate::runtime::interaction::GatewayInteractionCoordinator;
use crate::runtime::project::RuntimeError;
use crate::runtime::refresh::BoxFuture;
use crate::runtime::session::sidecar_session::{
    SidecarSession, SidecarSessionConfig, SidecarSpawner,
};
use crate::runtime::session::{
    LiveSession, LiveSessionHandle, LiveTurn, SessionContext, SessionFactory, SessionNext,
    SessionOwner, SessionOwnerOptions, SessionQueuedTurn,
    routes::{
        SessionRouteOptions, SessionRoutes,
        host::{LiveBindings, NativeTranscriptHost},
        live::LiveSessionState,
        store::SessionTranscriptStore,
    },
};
use crate::storage::{
    AsyncTranscriptAdapter, FileRunRegistry, SessionStorageInput, recorder::Recorder,
};
use pilotdeck_protocol::{JsObject, JsString, JsValue};
use sha2::{Digest, Sha256};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicU64, Ordering},
};
use std::{collections::VecDeque, path::PathBuf};
use tokio::process::Command;

/// Methods this runtime answers itself. With the TS gateway gone a forward has
/// no destination, so everything else is an explicit error.
pub const NATIVE_METHODS: &[&str] = &[
    "submit_turn",
    "abort_turn",
    "steer_turn",
    "cancel_steer",
    "session_model_set",
    "session_model_clear",
    "session_model_get",
    "usage_snapshot",
    "model_usage_snapshot",
    "active_turn_snapshot",
    "read_session_messages",
    "run_get",
    "run_events",
    "run_reattach",
    "describe_server",
    "supported_agents",
    "list_projects",
    "describe_project",
    "project_files_list",
    "project_file_read",
    "model_catalog_list",
    "native_archive_manifest",
    "native_archive_entries",
    "native_archive_artifact",
    // Baseline capability flags name the archive family, not wire methods.
    "native_transcript_archive",
    "native_transcript_archive_artifact",
    "permission_list",
    "permission_decide",
    "elicitation_respond",
    "grant_session_permission",
    "manager_sessions",
    "manager_browsers",
];

pub struct GatewayComposition {
    pub owner: SessionOwner,
    pub sdk: Option<Arc<tokio::sync::Mutex<crate::runtime::sdk_session::SdkSessionRegistry>>>,
    pub turn: TurnGateway,
    pub interactions: Arc<GatewayInteractionCoordinator>,
    pub async_hooks: crate::runtime::async_hooks::AsyncHookRegistry,
    #[cfg(test)]
    memory_registry: crate::runtime::session::context_resources::ProjectMemoryRegistry,
}

pub struct CompositionOptions {
    pub pilot_home: PathBuf,
    pub cwd: JsString,
    pub spawner: Arc<dyn SidecarSpawner>,
    pub providers: Box<dyn FnOnce(SessionOwner) -> std::io::Result<NativeSubmitProviders> + Send>,
    pub failure_guard_enabled: bool,
    pub abort_turn_timeout_ms: u64,
    pub server_version: String,
    pub policies: crate::cron::native::SessionPolicies,
    pub activity: Arc<crate::cron::activity::Activity>,
    pub background: Arc<crate::cron::runtime::BackgroundService>,
    pub cron: Arc<std::sync::Mutex<std::sync::Weak<crate::cron::runtime::CronRuntime>>>,
}

#[derive(Clone, Default)]
pub struct HistoryForkPorts {
    pub session: Option<Arc<dyn crate::runtime::session::routes::history::ProjectSessionForkPort>>,
    pub execution:
        Option<Arc<dyn crate::runtime::session::routes::history::ExecutionStorageForkPort>>,
}

type SharedPluginResources = Arc<
    std::sync::Mutex<(
        Vec<Arc<crate::runtime::plugins::LoadedPlugin>>,
        Option<PathBuf>,
    )>,
>;

struct RuntimeRequests {
    metadata: Arc<crate::metadata::Metadata<crate::metadata::NativeFiles>>,
    mcp: Option<crate::runtime::mcp::routes::McpRoutes>,
    interactions: Arc<GatewayInteractionCoordinator>,
    reader: Arc<FileRegistry>,
    coordinator: Arc<TurnEventCoordinator>,
    sessions: Arc<SessionRoutes>,
    host: Arc<NativeSubmitHost>,
    usage: Arc<super::routes::usage::UsageRegistry>,
}

impl GatewayRequests for RuntimeRequests {
    fn describe_server(&self) -> Call<'_, Result<JsValue, RequestError>> {
        Box::pin(async move {
            let mut info = self.sessions.describe_server();
            let sdk_defaults = if let Some(sdk) = &self.host.providers.sdk {
                sdk.lock()
                    .await
                    .options()
                    .organization_policy
                    .as_ref()
                    .is_some_and(
                        crate::runtime::sdk_session::OrganizationPolicy::has_session_defaults,
                    )
            } else {
                false
            };
            if let JsValue::Object(fields) = &mut info {
                fields.insert(
                    "projectKey".into(),
                    self.metadata.cwd.to_string_lossy().as_ref().into(),
                );
                let capabilities = super::capabilities::ORDER
                    .iter()
                    .copied()
                    .filter(|name| {
                        match *name {
                            // These ports belong to the outer local-server composition.
                            "cron_create"
                            | "cron_list"
                            | "cron_update"
                            | "cron_delete"
                            | "cron_stop"
                            | "cron_run_now"
                            | "always_on_apply"
                            | "always_on_abort"
                            | "always_on_rerun_plan"
                            | "background_task_stop"
                            | "background_tasks"
                            | "async_hook_result"
                            | "user_dialog_list"
                            | "rewind_files" => false,
                            "sdk_session_defaults" => sdk_defaults,
                            "manager_browsers" => self.host.providers.manager_browsers.is_some(),
                            "mcp_server_status"
                            | "set_mcp_servers"
                            | "mcp_server_reconnect"
                            | "mcp_server_toggle"
                            | "set_mcp_permission_mode_override" => self.mcp.is_some(),
                            "sdk_session_config" => self.host.providers.sdk.is_some(),
                            _ => true,
                        }
                    })
                    .map(JsValue::from)
                    .collect();
                fields.insert("capabilities".into(), JsValue::Array(capabilities));
            }
            Ok(info)
        })
    }

    fn request(
        &self,
        method: JsString,
        params: JsValue,
    ) -> Call<'_, Result<JsValue, RequestError>> {
        if let Some(name) = method
            .to_utf8()
            .ok()
            .filter(|name| crate::routes::domains::METHODS.contains(&name.as_str()))
        {
            return Box::pin(crate::routes::domains::request(
                self.metadata.clone(),
                name,
                params,
            ));
        }
        if method
            .to_utf8()
            .ok()
            .is_some_and(|name| crate::routes::is_metadata_method(&name))
        {
            return Box::pin(crate::routes::native_metadata(
                method,
                params,
                self.metadata.clone(),
            ));
        }
        if let Some(name) = method
            .to_utf8()
            .ok()
            .filter(|name| crate::runtime::mcp::routes::McpRoutes::handles(name))
        {
            return Box::pin(async move {
                let routes = self.mcp.as_ref().ok_or_else(|| {
                    let mut error = RequestError::new(format!("{name} is unavailable."));
                    error.code = "CAPABILITY_UNAVAILABLE".into();
                    error
                })?;
                routes
                    .handle(&name, &params)
                    .expect("registered MCP method")
                    .await
                    .map_err(|error| {
                        let mut result = RequestError::new(error.message().clone());
                        result.code = error.code().unwrap_or("gateway_request_failed").into();
                        result
                    })
            });
        }
        let interactions = self.interactions.clone();
        let registry = self.reader.clone();
        let coordinator = self.coordinator.clone();
        Box::pin(async move {
            let name = method.to_utf8().unwrap_or_default();
            if let Some(result) =
                crate::runtime::permission::routes::handle(&name, &params, &interactions).or_else(
                    || crate::runtime::interaction::routes::handle(&name, &params, &interactions),
                )
            {
                return result;
            }
            if let Some(request) = self.sessions.handle(&name, &params) {
                return request.await;
            }
            let session = params
                .get("sessionKey")
                .and_then(JsValue::as_string)
                .cloned()
                .unwrap_or_default();
            match name.as_str() {
                "manager_browsers" => {
                    self.host
                        .providers
                        .manager_browsers
                        .as_ref()
                        .ok_or_else(crate::runtime::manager::browser_provider_unavailable)?
                        .browsers(&self.metadata.home, &self.metadata.cwd, &params)
                        .await
                }
                "manager_sessions" => {
                    crate::runtime::manager::sessions(
                        &self.metadata.home,
                        &self.metadata.cwd,
                        &params,
                    )
                    .await
                }
                "steer_turn" | "cancel_steer" => {
                    let composer = super::routes::control::ProductionSteerTurnComposer::new(
                        PathBuf::from(self.host.cwd.to_utf8().unwrap_or_default()),
                    );
                    let routes =
                        super::routes::control::ControlRoutes::new(&self.host.owner, &composer);
                    routes.handle(&name, &params).expect("control method").await
                }
                "session_model_set" | "session_model_clear" | "session_model_get" => {
                    let sdk = self
                        .host
                        .providers
                        .sdk
                        .as_ref()
                        .ok_or_else(|| RequestError::new("Native SDK registry is required."))?;
                    super::routes::models::handle(
                        &super::routes::models::Models {
                            sdk,
                            owner: &self.host.owner,
                            replacement: &self.host.providers.replacement,
                            transcripts: &self.host.providers.transcripts,
                            pilot_home: &self.host.providers.pilot_home,
                        },
                        &name,
                        &params,
                    )
                    .await
                }
                "usage_snapshot" | "model_usage_snapshot" => {
                    let home = self.host.providers.pilot_home.clone();
                    let cwd = self.host.cwd.clone();
                    let usage = self.usage.clone();
                    tokio::task::spawn_blocking(move || {
                        let resolve = |project: Option<&JsString>| {
                            let snapshot = load_project_snapshot(&home, &cwd, project)?;
                            usage.resolve_with_snapshot(project, &snapshot)
                        };
                        super::routes::usage::UsageRoutes::new(&resolve)
                            .handle(&name, params)
                            .expect("usage method")
                    })
                    .await
                    .map_err(|error| RequestError::new(error.to_string()))?
                }
                "active_turn_snapshot" | "run_get" | "run_events" | "run_reattach" => {
                    super::routes::reads::handle(
                        coordinator.as_ref(),
                        registry.as_ref(),
                        &name,
                        &params,
                    )
                    .await
                }
                "read_session_messages" => {
                    read::read_session_messages(coordinator.as_ref(), None, &params)
                        .await
                        .map_err(super::routes::reads::request_error)
                }
                "abort_turn" => {
                    // TurnGateway intercepts this before delegating; keep a
                    // session-wide fallback for direct calls.
                    let reason = params
                        .get("reason")
                        .and_then(JsValue::as_string)
                        .cloned()
                        .unwrap_or_else(|| JsString::from("aborted"));
                    interactions.reject_pending_turn(&session, &reason);
                    Ok(object([("ok", JsValue::Bool(true))]))
                }
                _ => {
                    let home = self.host.providers.pilot_home.clone();
                    let cwd = PathBuf::from(
                        self.host
                            .cwd
                            .to_utf8()
                            .map_err(|error| RequestError::new(error.to_string()))?,
                    );
                    tokio::task::spawn_blocking(move || {
                        let request = object([
                            ("type", "request".into()),
                            ("id", "local".into()),
                            ("method", JsValue::String(method.clone())),
                            ("params", params),
                        ]);
                        let metadata = crate::metadata::Metadata {
                            home,
                            cwd,
                            files: crate::metadata::NativeFiles,
                        };
                        let crate::dispatch::Route::LocalResponse(bytes) =
                            crate::dispatch::route_with_metadata(&request, Some(&metadata))
                        else {
                            return Err(RequestError {
                                code: "unknown_method".into(),
                                ..RequestError::new({
                                    let mut units = "Unknown gateway method "
                                        .encode_utf16()
                                        .collect::<Vec<_>>();
                                    units.extend_from_slice(method.units());
                                    units.push(46);
                                    JsString::from_utf16(units)
                                })
                            });
                        };
                        let response = crate::dispatch::parse(&bytes)
                            .ok_or_else(|| RequestError::new("Invalid local metadata response."))?;
                        if response.get("ok") == Some(&JsValue::Bool(true)) {
                            Ok(response
                                .get("result")
                                .cloned()
                                .unwrap_or(JsValue::Undefined))
                        } else {
                            let error = response.get("error").cloned().unwrap_or(JsValue::Null);
                            Err(RequestError {
                                code: error
                                    .get("code")
                                    .and_then(JsValue::as_string)
                                    .and_then(|value| value.to_utf8().ok())
                                    .unwrap_or_else(|| "gateway_request_failed".into()),
                                message: error
                                    .get("message")
                                    .and_then(JsValue::as_string)
                                    .cloned()
                                    .unwrap_or_else(|| "Local metadata request failed.".into()),
                                details: error.get("details").cloned().map(Box::new),
                                validation: error.get("validation").cloned().map(Box::new),
                            })
                        }
                    })
                    .await
                    .map_err(|error| RequestError::new(error.to_string()))?
                }
            }
        })
    }
}

/// Real sidecar binary, spawned per turn when `PILOTDECK_AGENT_SUBPROCESS=1`.
/// Default production wiring uses the embedded in-process agent worker instead.
pub struct BinarySidecarSpawner {
    pub executable: PathBuf,
    pub cwd: PathBuf,
}

impl BinarySidecarSpawner {
    fn connect_with_env(
        &self,
        limits: crate::host::Limits,
        mut env: std::collections::HashMap<String, String>,
    ) -> crate::host::HostFuture<Option<crate::host::HostClient>> {
        if std::env::var("PILOTDECK_AGENT_SUBPROCESS").as_deref() == Ok("1")
            && env
                .get(crate::host::deployment::TRANSPORT)
                .is_none_or(|value| value.trim().is_empty())
        {
            env.insert(crate::host::deployment::TRANSPORT.into(), "stdio".into());
        }
        let profile = crate::host::deployment::resolve(&env, &self.cwd, &self.executable);
        let cwd = self.cwd.clone();
        Box::pin(async move {
            match profile.map_err(|message| {
                crate::host::HostError::new(crate::host::HostErrorKind::Protocol, message)
            })? {
                crate::host::deployment::Profile::Native => Ok(None),
                crate::host::deployment::Profile::Stdio { command, args, env } => {
                    let mut child = Command::new(command);
                    child.args(args).current_dir(cwd).env_clear().envs(env);
                    crate::host::HostClient::spawn(
                        &mut child,
                        Some(&"pilotdeck-agent-loop".into()),
                        limits,
                    )
                    .await
                    .map(Some)
                }
                crate::host::deployment::Profile::Tcp {
                    host,
                    port,
                    connect_timeout,
                } => crate::host::HostClient::connect_tcp(
                    &host,
                    port,
                    connect_timeout,
                    Some(&"pilotdeck-agent-loop".into()),
                    limits,
                )
                .await
                .map(Some),
            }
        })
    }
}

impl SidecarSpawner for BinarySidecarSpawner {
    fn command(&self) -> std::io::Result<Command> {
        let mut command = Command::new(&self.executable);
        command.arg("--stdio").current_dir(&self.cwd);
        Ok(command)
    }

    fn connect(
        &self,
        limits: crate::host::Limits,
    ) -> crate::host::HostFuture<Option<crate::host::HostClient>> {
        self.connect_with_env(limits, std::env::vars().collect())
    }
}

pub fn compose(options: CompositionOptions) -> std::io::Result<GatewayComposition> {
    compose_with_user_dialogs(options, Arc::default())
}

pub fn compose_with_fork_ports(
    options: CompositionOptions,
    fork_ports: HistoryForkPorts,
) -> std::io::Result<GatewayComposition> {
    compose_with_user_dialogs_and_fork_ports(options, Arc::default(), fork_ports)
}

pub fn compose_with_user_dialogs_and_fork_ports(
    options: CompositionOptions,
    user_dialogs: Arc<crate::user_dialog::GatewayUserDialogs>,
    fork_ports: HistoryForkPorts,
) -> std::io::Result<GatewayComposition> {
    compose_with_user_dialogs_impl(options, user_dialogs, fork_ports)
}

pub fn compose_with_user_dialogs(
    options: CompositionOptions,
    user_dialogs: Arc<crate::user_dialog::GatewayUserDialogs>,
) -> std::io::Result<GatewayComposition> {
    compose_with_user_dialogs_impl(options, user_dialogs, HistoryForkPorts::default())
}

fn compose_with_user_dialogs_impl(
    options: CompositionOptions,
    user_dialogs: Arc<crate::user_dialog::GatewayUserDialogs>,
    fork_ports: HistoryForkPorts,
) -> std::io::Result<GatewayComposition> {
    let process_env: crate::config::Env = std::env::vars().collect();
    let runtime_env = crate::config::runtime_env::resolve(&process_env);
    let registry = open_registry(&options.pilot_home)?;
    let metadata = Arc::new(crate::metadata::Metadata {
        home: options.pilot_home.clone(),
        cwd: PathBuf::from(options.cwd.to_utf8().map_err(std::io::Error::other)?),
        files: crate::metadata::NativeFiles,
    });
    let permissions =
        Arc::new(crate::config::permission::GatewaySessionPermissionRuleSetRegistry::new());
    let sdk_slot: Arc<
        std::sync::Mutex<
            Option<Arc<tokio::sync::Mutex<crate::runtime::sdk_session::SdkSessionRegistry>>>,
        >,
    > = Arc::new(std::sync::Mutex::new(None));
    let hook_sdk = sdk_slot.clone();
    let interactions = Arc::new(GatewayInteractionCoordinator::new(
        crate::runtime::interaction::InteractionOptions {
            permission_grants: Some(permissions.clone()),
            on_elicitation_delivered: Some(Arc::new(move |session, request| {
                let registry = hook_sdk.lock().unwrap().clone();
                let session = session.clone();
                let request = request.clone();
                if let Some(registry) = registry {
                    tokio::spawn(async move {
                        let handler = registry
                            .lock()
                            .await
                            .handlers()
                            .session_hook_handler(&session)
                            .cloned();
                        if let Some(handler) = handler {
                            handler(
                                crate::runtime::sdk_session::PilotDeckHookEvent::ElicitationResult,
                                &object([
                                    ("requestId", JsValue::String(request)),
                                    ("delivered", JsValue::Bool(true)),
                                ]),
                            );
                        }
                    });
                }
            })),
            ..Default::default()
        },
    ));
    let cwd_path = std::path::PathBuf::from(options.cwd.to_utf8().map_err(std::io::Error::other)?);
    let pilot_home = crate::metadata::resolve_process_path(&cwd_path, &options.pilot_home);
    let layout_cwd = cwd_path.clone();
    let adapter = Arc::new(AsyncTranscriptAdapter::new(
        Arc::new(SessionTranscriptStore::new(
            pilot_home.clone(),
            cwd_path.clone(),
        )),
        Arc::new(move |project| {
            crate::session_chat_dir(
                &project.pilot_home,
                &layout_cwd,
                &project.project_root.to_string_lossy(),
            )
        }),
    ));
    let live = Arc::new(LiveBindings::default());
    let history_storage = Arc::new(
        crate::runtime::session::routes::history::HistoryStorage::new(
            adapter.clone(),
            pilot_home.clone(),
            cwd_path.clone(),
            Arc::new(|| {
                let epoch = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis() as i64;
                pilotdeck_transport::system_clock::format_epoch_iso(epoch)
            }),
            None,
            None,
        )
        .with_fork_ports(fork_ports.session, fork_ports.execution),
    );
    let snapshot =
        crate::config::load_snapshot(&options.pilot_home, &cwd_path, &process_env, None, 1.0, "");
    let usage = Arc::new(super::routes::usage::UsageRegistry::new(
        options.pilot_home.clone(),
        options.cwd.clone(),
    ));
    if let Ok(snapshot) = snapshot {
        usage
            .resolve_with_snapshot(None, &snapshot)
            .map_err(|error| {
                std::io::Error::other(String::from_utf16_lossy(error.message.units()))
            })?;
    }
    let mcp_config = Arc::new(std::sync::Mutex::new(None));
    let async_hook_bindings = crate::runtime::async_hooks::LiveAsyncHookBindings::default();
    let async_hooks = async_hook_bindings.registry.clone();
    let factory_mcp = mcp_config.clone();
    let memory = Arc::new(std::sync::Mutex::new(None));
    let factory_memory = memory.clone();
    let sdk_registry = Arc::new(std::sync::Mutex::new(None));
    let factory_sdk_registry = sdk_registry.clone();
    let plugin_resources: SharedPluginResources = Arc::new(std::sync::Mutex::new((vec![], None)));
    let factory_plugin_resources = plugin_resources.clone();
    let continuation_runtime =
        crate::runtime::session::subagent_continuation::NativeContinuationRuntime::new()
            .map_err(std::io::Error::other)?;
    let continuation_owner = Arc::new(Mutex::new(None));
    let factory_continuation_owner = continuation_owner.clone();
    let memory_registry =
        crate::runtime::session::context_resources::ProjectMemoryRegistry::default();
    let factory = Arc::new(NativeSessionFactory {
        sdk: sdk_slot.clone(),
        async_hook_bindings,
        user_dialogs,
        mcp_config: factory_mcp,
        memory: factory_memory,
        sdk_registry: factory_sdk_registry,
        plugin_resources: factory_plugin_resources,
        permissions: permissions.clone(),
        interactions: interactions.clone(),
        limits: crate::host::Limits::default(),
        spawner: options.spawner,
        cwd: options.cwd.clone(),
        pilot_home: pilot_home.clone(),
        cwd_path: cwd_path.clone(),
        adapter: adapter.clone(),
        live: live.clone(),
        history_storage: history_storage.clone(),
        usage: usage.clone(),
        router_generations: Default::default(),
        policies: options.policies.clone(),
        background: options.background.clone(),
        cron: options.cron.clone(),
        continuation_runtime,
        continuation_owner: factory_continuation_owner,
        next_continuation_identity: AtomicU64::new(1),
        memory_registry: memory_registry.clone(),
    });
    let interactions_slot = interactions.clone();
    let registry_slot = registry.clone();
    let bindings = Arc::new(std::sync::Mutex::new(None));
    let bound = bindings.clone();
    let turn = TurnGateway::start(TurnReplayOptions::new(), move |coordinator| {
        let owner = SessionOwner::new(SessionOwnerOptions::new(factory));
        *continuation_owner.lock().unwrap() = Some(owner.clone());
        let mut providers = (options.providers)(owner.clone())?;
        *memory.lock().unwrap() = providers.memory.clone();
        let sdk = providers.sdk.clone();
        *sdk_slot.lock().unwrap() = sdk.clone();
        *bound.lock().unwrap() = Some((owner.clone(), sdk.clone()));
        let transcript_host = Arc::new(NativeTranscriptHost {
            owner: owner.clone(),
            adapter: adapter.clone(),
            home: pilot_home.clone(),
            cwd: cwd_path.clone(),
            live: live.clone(),
            sdk: sdk.clone(),
        });
        let sessions = Arc::new(SessionRoutes::new(SessionRouteOptions {
            home: pilot_home,
            cwd: cwd_path,
            owner: owner.clone(),
            interactions: interactions_slot.clone(),
            coordinator: coordinator.clone(),
            adapter,
            history_storage,
            live: live.clone(),
            transcript_host,
            run_registry: true,
            on_close: Arc::new(move |session, reason| {
                if reason.equals_str("session_deleted") {
                    live.clear(session);
                    if let Some(sdk) = &sdk {
                        let mut sdk = sdk.try_lock().map_err(|_| {
                            crate::runtime::session::routes::host::error(
                                "SESSION_BUSY",
                                "Session SDK state is being updated.",
                            )
                        })?;
                        sdk.clear_session_state(session).map_err(|failure| {
                            crate::runtime::session::routes::host::error(
                                failure.code(),
                                failure.message(),
                            )
                        })?;
                    }
                }
                Ok(())
            }),
        }));
        providers.replacement = sessions.replacement.clone();
        let history = sessions.history_storage.clone();
        providers.recover_history = Some(Arc::new(move |input| {
            history.recover_replacements(input).map_err(|error| {
                super::submit::SubmitError::new(
                    error.code.unwrap_or("gateway_submit_failed"),
                    error.message,
                )
            })
        }));
        let mcp = providers.sdk.as_ref().map(|sdk| {
            crate::runtime::mcp::native::routes(
                owner.clone(),
                sdk.clone(),
                interactions_slot.clone(),
                permissions.clone(),
            )
        });
        *mcp_config.lock().unwrap() = mcp.as_ref().map(|routes| routes.sdk_config());
        let host = Arc::new(NativeSubmitHost {
            owner: owner.clone(),
            interactions: interactions_slot.clone(),
            providers,
            failure_guard_enabled: options.failure_guard_enabled,
            abort_turn_timeout_ms: options.abort_turn_timeout_ms,
            cwd: options.cwd.clone(),
            policies: options.policies.clone(),
            activity: options.activity.clone(),
            max_output_tokens: runtime_env.max_output_tokens,
        });
        let submitter = Arc::new(TurnSubmitter {
            owner,
            coordinator: coordinator.clone(),
            registry: Some(registry_slot.clone()),
            projector: Arc::new(GatewayAgentEventProjector::default()),
            host: host.clone(),
            workspace_admissions: WorkspaceAdmissions::default(),
        });
        Ok(GatewayServices {
            submitter,
            interactions: interactions_slot.clone(),
            requests: Arc::new(RuntimeRequests {
                metadata,
                mcp,
                interactions: interactions_slot.clone(),
                reader: registry_slot.clone(),
                coordinator,
                sessions,
                host: host.clone(),
                usage,
            }),
        })
    })?;
    let (owner, sdk) = bindings
        .lock()
        .unwrap()
        .take()
        .expect("initialized native host");
    Ok(GatewayComposition {
        turn,
        interactions,
        owner,
        sdk,
        async_hooks,
        #[cfg(test)]
        memory_registry,
    })
}

fn open_registry(pilot_home: &std::path::Path) -> std::io::Result<Arc<FileRegistry>> {
    let dir = pilot_home.join("gateway");
    std::fs::create_dir_all(&dir)?;
    let file = FileRunRegistry::open_journaled(dir.join("runs.json"))
        .map_err(|error| std::io::Error::other(error.to_string()))?;
    Ok(Arc::new(FileRegistry::new(file)?))
}

fn load_project_snapshot(
    pilot_home: &std::path::Path,
    cwd: &JsString,
    project: Option<&JsString>,
) -> Result<JsValue, RequestError> {
    let cwd = PathBuf::from(
        cwd.to_utf8()
            .map_err(|error| RequestError::new(error.to_string()))?,
    );
    let project = project
        .map(JsString::to_utf8)
        .transpose()
        .map_err(|error| RequestError::new(error.to_string()))?;
    crate::config::load_snapshot(
        pilot_home,
        &cwd,
        &std::env::vars().collect(),
        project.as_deref(),
        1.0,
        "",
    )
    .map_err(|error| RequestError {
        code: error.code,
        message: error.message,
        details: None,
        validation: None,
    })
}

/// One sidecar-backed session per session key.
fn skill_names(value: &JsValue) -> Option<Vec<String>> {
    let JsValue::Array(values) = value else {
        return None;
    };
    Some(
        values
            .iter()
            .filter_map(JsValue::as_string)
            .map(|value| String::from_utf16_lossy(value.units()))
            .collect(),
    )
}

fn stage_native_memory_for_session(
    project_root: &std::path::Path,
    snapshot: &JsValue,
    pilot_home: &std::path::Path,
    has_injected_resolver: bool,
) -> Result<Option<crate::runtime::memory::edgeclaw::NativeProjectMemoryOwner>, RuntimeError> {
    if has_injected_resolver {
        return Ok(None);
    }
    let llm = crate::runtime::memory::edgeclaw::native_llm_from_snapshot(snapshot);
    crate::runtime::memory::edgeclaw::stage_native_from_snapshot(
        project_root,
        snapshot,
        pilot_home,
        llm,
    )
    .map_err(|error| RuntimeError::new(String::from_utf16_lossy(error.message.units())))
}

fn native_memory_fingerprint(snapshot: &JsValue) -> Result<String, RuntimeError> {
    let undefined = JsValue::Undefined;
    let config = snapshot.get("config").unwrap_or(&undefined);
    let relevant = object([
        (
            "memory",
            config.get("memory").cloned().unwrap_or(JsValue::Undefined),
        ),
        (
            "agentModel",
            config
                .get("agent")
                .and_then(|agent| agent.get("model"))
                .cloned()
                .unwrap_or(JsValue::Undefined),
        ),
        (
            "model",
            config.get("model").cloned().unwrap_or(JsValue::Undefined),
        ),
    ]);
    let encoded = pilotdeck_protocol::stringify_json(&relevant)
        .map_err(|error| RuntimeError::new(error.to_string()))?
        .ok_or_else(|| RuntimeError::new("Native memory config is not serializable."))?;
    Ok(format!("{:x}", Sha256::digest(encoded.as_bytes())))
}

struct NativeSessionFactory {
    sdk: Arc<
        std::sync::Mutex<
            Option<Arc<tokio::sync::Mutex<crate::runtime::sdk_session::SdkSessionRegistry>>>,
        >,
    >,
    async_hook_bindings: crate::runtime::async_hooks::LiveAsyncHookBindings,
    user_dialogs: Arc<crate::user_dialog::GatewayUserDialogs>,
    mcp_config: Arc<std::sync::Mutex<Option<Arc<crate::runtime::mcp::sdk::SdkMcpConfig>>>>,
    memory: Arc<std::sync::Mutex<Option<Arc<dyn crate::runtime::context::memory::MemoryResolver>>>>,
    sdk_registry: Arc<
        std::sync::Mutex<
            Option<Arc<tokio::sync::Mutex<crate::runtime::sdk_session::SdkSessionRegistry>>>,
        >,
    >,
    plugin_resources: SharedPluginResources,
    permissions: Arc<crate::config::permission::GatewaySessionPermissionRuleSetRegistry>,
    interactions: Arc<GatewayInteractionCoordinator>,
    history_storage: Arc<crate::runtime::session::routes::history::HistoryStorage>,
    limits: crate::host::Limits,
    spawner: Arc<dyn SidecarSpawner>,
    cwd: JsString,
    pilot_home: PathBuf,
    cwd_path: PathBuf,
    adapter: Arc<AsyncTranscriptAdapter>,
    live: Arc<LiveBindings>,
    usage: Arc<super::routes::usage::UsageRegistry>,
    router_generations: Arc<crate::runtime::model::router::RouterGenerationRegistry>,
    policies: crate::cron::native::SessionPolicies,
    background: Arc<crate::cron::runtime::BackgroundService>,
    cron: Arc<std::sync::Mutex<std::sync::Weak<crate::cron::runtime::CronRuntime>>>,
    continuation_runtime: crate::runtime::session::subagent_continuation::NativeContinuationRuntime,
    continuation_owner: Arc<Mutex<Option<SessionOwner>>>,
    next_continuation_identity: AtomicU64,
    memory_registry: crate::runtime::session::context_resources::ProjectMemoryRegistry,
}

struct ContinuationParentSession {
    inner: Arc<dyn LiveSession>,
    queued_input_template: JsValue,
    installed: Mutex<
        Option<
            crate::runtime::tools::builtin::subagent_continuation::InstalledSubagentContinuation,
        >,
    >,
    tool_binding: Mutex<Option<crate::runtime::session::module_host::LiveContinuationToolBinding>>,
    disposed: AtomicBool,
}

impl ContinuationParentSession {
    fn new(inner: Arc<dyn LiveSession>, queued_input_template: JsValue) -> Self {
        Self {
            inner,
            queued_input_template,
            installed: Mutex::new(None),
            tool_binding: Mutex::new(None),
            disposed: AtomicBool::new(false),
        }
    }

    fn install(
        &self,
        installed: crate::runtime::tools::builtin::subagent_continuation::InstalledSubagentContinuation,
        tool_binding: crate::runtime::session::module_host::LiveContinuationToolBinding,
    ) {
        *self.installed.lock().unwrap() = Some(installed);
        *self.tool_binding.lock().unwrap() = Some(tool_binding);
    }
}

impl LiveSession for ContinuationParentSession {
    fn steer(
        &self,
        turn_id: JsString,
        message: crate::runtime::steer::SteerMessage,
    ) -> crate::host::HostFuture<crate::runtime::steer::EnqueueResult> {
        self.inner.steer(turn_id, message)
    }

    fn cancel_steer(
        &self,
        turn_id: JsString,
        item_id: JsString,
    ) -> crate::host::HostFuture<crate::runtime::steer::CancelResult> {
        self.inner.cancel_steer(turn_id, item_id)
    }

    fn submit(&self, input: JsValue, options: JsValue) -> Result<Box<dyn LiveTurn>, RuntimeError> {
        self.inner.submit(input, options)
    }

    fn queued_input_template(&self) -> Option<JsValue> {
        Some(self.queued_input_template.clone())
    }

    fn snapshot(&self) -> JsValue {
        self.inner.snapshot()
    }

    fn abort(&self, reason: &JsString) -> crate::runtime::project::RuntimeResult {
        self.inner.abort(reason)
    }

    fn dispose_start(&self, reason: &JsString) -> crate::runtime::project::DisposeFuture<'_> {
        let reason = reason.clone();
        let installed = self.installed.lock().unwrap().take();
        Box::pin(async move {
            match installed {
                Some(installed) => installed
                    .dispose(reason)
                    .await
                    .map_err(|error| RuntimeError::new(error.message)),
                None => Ok(()),
            }
        })
    }

    fn dispose(&self) -> crate::runtime::project::DisposeFuture<'_> {
        Box::pin(async move {
            if self.disposed.swap(true, Ordering::AcqRel) {
                return Ok(());
            }
            let installed = self.installed.lock().unwrap().take();
            let mut failures = Vec::new();
            if let Some(installed) = installed
                && let Err(error) = installed.dispose("parent_agent_disposed".into()).await
            {
                failures.push(RuntimeError::new(error.message));
            }
            drop(self.tool_binding.lock().unwrap().take());
            if let Err(error) = self.inner.dispose().await {
                failures.push(error);
            }
            runtime_failures("Failed to dispose continuation parent session.", failures)
        })
    }
}

fn runtime_failures(message: &str, mut failures: Vec<RuntimeError>) -> Result<(), RuntimeError> {
    match failures.len() {
        0 => Ok(()),
        1 => Err(failures.remove(0)),
        _ => Err(RuntimeError {
            message: message.into(),
            causes: failures,
        }),
    }
}

struct ProductionContinuationSessionFactory {
    parent_session_id: JsString,
    project_key: Option<JsString>,
    project_root: PathBuf,
    pilot_home: PathBuf,
    cwd: JsString,
    snapshot: JsValue,
    services: Option<Arc<crate::cron::native::NativeSessionServices>>,
    spawner: Arc<dyn SidecarSpawner>,
    parent_live: Arc<LiveSessionState>,
    adapter: Arc<AsyncTranscriptAdapter>,
    context_resources: Arc<crate::runtime::session::context_resources::ContextResources>,
    interactions: Arc<GatewayInteractionCoordinator>,
    stats: Option<super::routes::usage::SharedUsageStats>,
    parent_agent: JsObject,
    continuation_runtime: crate::runtime::session::subagent_continuation::NativeContinuationRuntime,
}

impl ProductionContinuationSessionFactory {
    fn read_child(&self, child_session_id: &JsString) -> Result<Vec<JsValue>, RuntimeError> {
        let storage = self.adapter.create_session_storage(SessionStorageInput {
            project_root: self.project_root.clone(),
            pilot_home: self.pilot_home.clone(),
            session_id: self.parent_session_id.clone(),
            now: None,
        });
        let parent = self.parent_live.recorder.writer();
        let path = parent
            .path()
            .parent()
            .unwrap_or_else(|| std::path::Path::new("/"))
            .join(parent.relative_subagent_path(child_session_id));
        storage
            .read_transcript_at_path(&path, None)
            .map(|read| read.entries)
            .map_err(|error| RuntimeError::new(error.to_string()))
    }
}

impl crate::runtime::session::subagent_continuation::NativeContinuationSessionFactory
    for ProductionContinuationSessionFactory
{
    fn materialize(
        &self,
        request: crate::runtime::session::subagent_continuation::NativeContinuationMaterializeRequest,
    ) -> crate::runtime::session::subagent_continuation::ContinuationFuture<
        crate::runtime::session::subagent_continuation::NativeContinuationMaterializedChild,
    > {
        let cancelled = request
            .cancellation
            .as_ref()
            .is_some_and(crate::runtime::network::Cancellation::is_cancelled);
        let existing = request
            .resume
            .then(|| self.read_child(&request.child_session_id));
        let sidechain = self
            .parent_live
            .recorder
            .writer()
            .for_subagent(&request.child_session_id);
        let writer = Arc::new(sidechain.writer);
        let restored = existing.transpose();
        let state = restored.and_then(|entries| {
            let queued = if let Some(entries) = entries {
                Recorder::from_writer(writer.clone())
                    .restore(&entries)
                    .map_err(|error| RuntimeError::new(error.to_string()))?;
                SessionQueuedTurn::restore(&entries)?
            } else {
                Vec::new()
            };
            crate::host::modules::tool_context::HostToolCheckpoint::new(None, &[])
                .map(|checkpoint| (checkpoint, queued))
                .map_err(|error| RuntimeError::new(error.to_string()))
        });
        let input_template = continuation_child_input(
            &self.parent_agent,
            &request.descriptor,
            &request.child_session_id,
        );
        let child_agent = input_template
            .get("agent")
            .and_then(JsValue::as_object)
            .cloned();
        let child_model = child_agent.as_ref().and_then(continuation_model);
        let config = SidecarSessionConfig {
            limits: crate::host::Limits::default(),
            session_key: request.child_session_id.clone(),
            project_key: self.project_key.clone(),
            cwd: self.cwd.clone(),
            snapshot: self.snapshot.clone(),
            services: self.services.clone(),
        };
        let spawner = self.spawner.clone();
        let parent_live = self.parent_live.clone();
        let context_resources = self.context_resources.clone();
        let interactions = self.interactions.clone();
        let stats = self.stats.clone();
        let descendant_parent_session_id = self.parent_session_id.clone();
        let descendant_project_key = self.project_key.clone();
        let descendant_project_root = self.project_root.clone();
        let descendant_pilot_home = self.pilot_home.clone();
        let descendant_cwd = self.cwd.clone();
        let descendant_snapshot = self.snapshot.clone();
        let descendant_services = self.services.clone();
        let descendant_spawner = self.spawner.clone();
        let descendant_adapter = self.adapter.clone();
        let descendant_interactions = self.interactions.clone();
        let descendant_stats = self.stats.clone();
        let continuation_runtime = self.continuation_runtime.clone();
        let child_session_id = request.child_session_id;
        Box::pin(async move {
            if cancelled {
                return Err(
                    crate::runtime::session::subagent_continuation::ContinuationError::new(
                        "subagent_admission_aborted",
                        "Continuable subagent materialization aborted: aborted",
                    ),
                );
            }
            let (checkpoint, queued) = state.map_err(|error| {
                crate::runtime::session::subagent_continuation::ContinuationError::new(
                    "subagent_materialize_failed",
                    error.message,
                )
            })?;
            let live = LiveSessionState::for_subagent(
                child_session_id.clone(),
                config.cwd.clone(),
                writer.clone(),
                checkpoint,
                parent_live.file_history.clone(),
            );
            let child_resources = Arc::new(context_resources.for_subagent(None, false));
            let mut session = SidecarSession::with_transcript(config, spawner, writer.clone())
                .with_context_resources(child_resources.clone())
                .with_interactions(interactions)
                .with_subagent_providers(continuation_runtime.providers());
            if let Some(stats) = stats {
                session = session.with_usage_stats(stats);
            }
            let session = Arc::new(ContinuationChildSession {
                inner: Arc::new(session),
                live: live.clone(),
                queued: Mutex::new(queued.into()),
                input_template: Mutex::new(JsValue::Object(input_template.clone())),
            });
            let configurator = match (child_agent, child_model) {
                (Some(parent_agent), Some((agent_provider, agent_model))) => {
                    let descendant_factory = Arc::new(ProductionContinuationSessionFactory {
                        parent_session_id: descendant_parent_session_id,
                        project_key: descendant_project_key,
                        project_root: descendant_project_root,
                        pilot_home: descendant_pilot_home,
                        cwd: descendant_cwd,
                        snapshot: descendant_snapshot,
                        services: descendant_services,
                        spawner: descendant_spawner,
                        parent_live: live.clone(),
                        adapter: descendant_adapter,
                        context_resources: child_resources,
                        interactions: descendant_interactions,
                        stats: descendant_stats,
                        parent_agent,
                        continuation_runtime: continuation_runtime.clone(),
                    });
                    Some(Arc::new(ProductionContinuationChildConfigurator {
                        runtime: continuation_runtime,
                        live,
                        descendant_factory,
                        agent_provider,
                        agent_model,
                    }) as Arc<dyn crate::runtime::session::subagent_continuation::NativeContinuationChildConfigurator>)
                }
                _ => None,
            };
            Ok(crate::runtime::session::subagent_continuation::NativeContinuationMaterializedChild {
                handle: LiveSessionHandle::new(session, None),
                transcript: writer,
                input_template,
                configurator,
            })
        })
    }

    fn inspect(
        &self,
        request: crate::runtime::session::subagent_continuation::NativeContinuationInspectRequest,
    ) -> crate::runtime::session::subagent_continuation::ContinuationFuture<Vec<JsValue>> {
        let result = self.read_child(&request.child_session_id).map_err(|error| {
            crate::runtime::session::subagent_continuation::ContinuationError::new(
                "subagent_inspect_failed",
                error.message,
            )
        });
        Box::pin(async move { result })
    }
}

struct ProductionContinuationChildConfigurator {
    runtime: crate::runtime::session::subagent_continuation::NativeContinuationRuntime,
    live: Arc<LiveSessionState>,
    descendant_factory: Arc<ProductionContinuationSessionFactory>,
    agent_provider: JsString,
    agent_model: JsString,
}

impl crate::runtime::session::subagent_continuation::NativeContinuationChildConfigurator
    for ProductionContinuationChildConfigurator
{
    fn configure(
        &self,
        child: crate::runtime::session::subagent_continuation::ContinuationParent,
        handle: LiveSessionHandle,
    ) -> crate::runtime::session::subagent_continuation::ContinuationFuture<
        Arc<dyn crate::runtime::session::subagent_continuation::NativeContinuationChildConfiguration>,
    >{
        let result = if handle
            .snapshot()
            .get("liveIdentity")
            .and_then(JsValue::as_string)
            != Some(&self.live.identity)
        {
            Err(
                crate::runtime::session::subagent_continuation::ContinuationError::new(
                    "invalid_subagent_parent",
                    "Nested continuation live identity does not match its child handle.",
                ),
            )
        } else {
            let notifier = Arc::new(ProductionContinuationParentNotifier {
                owner: None,
                handle,
                state: self.live.clone(),
            });
            crate::runtime::tools::builtin::subagent_continuation::install_for_parent(
                self.runtime.clone(),
                crate::runtime::tools::builtin::subagent_continuation::InstallSubagentContinuationRequest {
                    binding: crate::runtime::session::subagent_continuation::NativeContinuationParentBinding {
                        parent: child,
                        factory: self.descendant_factory.clone(),
                        notifier,
                    },
                    provider: "pilotdeck-native".into(),
                    agent_provider: self.agent_provider.clone(),
                    agent_model: self.agent_model.clone(),
                    definitions: continuation_definition_ids(),
                },
            )
            .map(|installed| {
                let tool_binding =
                    crate::runtime::session::module_host::bind_live_continuation_tools(
                        self.live.identity.clone(),
                        installed.port(),
                    );
                Arc::new(ProductionContinuationChildConfiguration {
                    installed: Mutex::new(Some(installed)),
                    tool_binding: Mutex::new(Some(tool_binding)),
                }) as Arc<dyn crate::runtime::session::subagent_continuation::NativeContinuationChildConfiguration>
            })
        };
        Box::pin(async move { result })
    }
}

struct ProductionContinuationChildConfiguration {
    installed: Mutex<
        Option<
            crate::runtime::tools::builtin::subagent_continuation::InstalledSubagentContinuation,
        >,
    >,
    tool_binding: Mutex<Option<crate::runtime::session::module_host::LiveContinuationToolBinding>>,
}

impl crate::runtime::session::subagent_continuation::NativeContinuationChildConfiguration
    for ProductionContinuationChildConfiguration
{
    fn dispose(
        &self,
        reason: JsString,
    ) -> crate::runtime::session::subagent_continuation::ContinuationFuture<()> {
        let installed = self.installed.lock().unwrap().take();
        let tool_binding = self.tool_binding.lock().unwrap().take();
        Box::pin(async move {
            let result = match installed {
                Some(installed) => installed.dispose(reason).await,
                None => Ok(()),
            };
            drop(tool_binding);
            result
        })
    }
}

struct ContinuationChildSession {
    inner: Arc<SidecarSession>,
    live: Arc<LiveSessionState>,
    queued: Mutex<VecDeque<SessionQueuedTurn>>,
    input_template: Mutex<JsValue>,
}

impl LiveSession for ContinuationChildSession {
    fn steer(
        &self,
        turn_id: JsString,
        message: crate::runtime::steer::SteerMessage,
    ) -> crate::host::HostFuture<crate::runtime::steer::EnqueueResult> {
        self.inner.steer(turn_id, message)
    }

    fn cancel_steer(
        &self,
        turn_id: JsString,
        item_id: JsString,
    ) -> crate::host::HostFuture<crate::runtime::steer::CancelResult> {
        self.inner.cancel_steer(turn_id, item_id)
    }

    fn submit(&self, input: JsValue, options: JsValue) -> Result<Box<dyn LiveTurn>, RuntimeError> {
        self.submit_inner(input, options, None)
    }

    fn pending_queued_turns(&self) -> Result<Vec<SessionQueuedTurn>, RuntimeError> {
        Ok(self.queued.lock().unwrap().iter().cloned().collect())
    }

    fn queued_input_template(&self) -> Option<JsValue> {
        Some(self.input_template.lock().unwrap().clone())
    }

    fn enqueue_queued_turn(
        &self,
        turn: &SessionQueuedTurn,
    ) -> crate::runtime::project::RuntimeResult {
        let mut queued = self.queued.lock().unwrap();
        self.live
            .recorder
            .record_turn_enqueued(
                &self.live.session_key,
                &turn.turn_id,
                &turn.item_id,
                turn.input.clone(),
                turn.submit_options.clone(),
            )
            .map_err(|error| RuntimeError::new(error.to_string()))?;
        queued.push_back(turn.clone());
        Ok(())
    }

    fn submit_queued_turn(
        &self,
        turn: &SessionQueuedTurn,
    ) -> Result<Box<dyn LiveTurn>, RuntimeError> {
        {
            let mut queued = self.queued.lock().unwrap();
            let next = queued
                .front()
                .filter(|next| next.item_id == turn.item_id && next.turn_id == turn.turn_id)
                .ok_or_else(|| RuntimeError::new("Queued turn is not the next FIFO admission."))?;
            self.live
                .recorder
                .start_queued_turn(&self.live.session_key, &next.turn_id, &next.item_id)
                .map_err(|error| RuntimeError::new(error.to_string()))?;
            queued.pop_front();
        }
        let mut options = turn
            .submit_options
            .as_object()
            .cloned()
            .ok_or_else(|| RuntimeError::new("Queued turn options must be an object."))?;
        options.insert("turnId".into(), JsValue::String(turn.turn_id.clone()));
        let input =
            continuation_queued_execution_input(&self.input_template.lock().unwrap(), &turn.input)?;
        self.submit_inner(input, JsValue::Object(options), Some(&turn.item_id))
    }

    fn discard_queued_turns(&self, reason: &str) -> crate::runtime::project::RuntimeResult {
        let mut queued = self.queued.lock().unwrap();
        while let Some(turn) = queued.front() {
            self.live
                .recorder
                .record_turn_discarded(&self.live.session_key, &turn.turn_id, &turn.item_id, reason)
                .map_err(|error| RuntimeError::new(error.to_string()))?;
            queued.pop_front();
        }
        Ok(())
    }

    fn snapshot(&self) -> JsValue {
        let mut snapshot = self
            .inner
            .snapshot()
            .as_object()
            .cloned()
            .unwrap_or_default();
        if let Ok(JsValue::Object(ref live)) = self.live.snapshot() {
            for (key, value) in live.entries() {
                snapshot.insert(key.clone(), value.clone());
            }
        }
        JsValue::Object(snapshot)
    }

    fn abort(&self, reason: &JsString) -> crate::runtime::project::RuntimeResult {
        self.inner.abort(reason)
    }

    fn dispose(&self) -> crate::runtime::project::DisposeFuture<'_> {
        Box::pin(async move {
            let inner = self.inner.dispose().await;
            let live = self.live.dispose();
            inner.and(live)
        })
    }
}

impl ContinuationChildSession {
    fn submit_inner(
        &self,
        mut input: JsValue,
        options: JsValue,
        inbox_item_id: Option<&JsString>,
    ) -> Result<Box<dyn LiveTurn>, RuntimeError> {
        let turn_id = options
            .get("turnId")
            .and_then(JsValue::as_string)
            .cloned()
            .filter(|turn| !turn.is_empty())
            .ok_or_else(|| RuntimeError::new("Turn identity is missing."))?;
        let JsValue::Object(fields) = &mut input else {
            return Err(RuntimeError::new("Session input must be an object."));
        };
        fields.insert("seedState".into(), self.live.seed_state()?);
        fields.insert(
            "sessionId".into(),
            JsValue::String(self.live.session_key.clone()),
        );
        fields.insert("turnId".into(), JsValue::String(turn_id.clone()));
        let accepted_message = fields
            .get("messages")
            .and_then(|messages| match messages {
                JsValue::Array(messages) => messages.last().cloned(),
                _ => None,
            })
            .ok_or_else(|| RuntimeError::new("Session input is empty."))?;
        let started = object([
            ("type", "turn_started".into()),
            ("sessionId", JsValue::String(self.live.session_key.clone())),
            ("turnId", JsValue::String(turn_id.clone())),
        ]);
        let mut started = started;
        if let (Some(item_id), JsValue::Object(fields)) = (inbox_item_id, &mut started) {
            fields.insert("inboxItemId".into(), JsValue::String(item_id.clone()));
        } else {
            self.live.record_event(&started)?;
        }
        let accepted = object([
            ("type", "input_accepted".into()),
            ("sessionId", JsValue::String(self.live.session_key.clone())),
            ("turnId", JsValue::String(turn_id)),
            ("messages", JsValue::Array(vec![accepted_message])),
        ]);
        self.live.record_event(&accepted)?;
        let inner =
            crate::runtime::session::module_host::with_live_state(self.live.clone(), || {
                self.inner.submit(input, options)
            })?;
        Ok(Box::new(ContinuationChildTurn {
            inner,
            started: Some(started),
            accepted: Some(accepted),
            live: self.live.clone(),
        }))
    }
}

struct ContinuationChildTurn {
    inner: Box<dyn LiveTurn>,
    started: Option<JsValue>,
    accepted: Option<JsValue>,
    live: Arc<LiveSessionState>,
}

impl LiveTurn for ContinuationChildTurn {
    fn next(&mut self) -> SessionNext<'_> {
        Box::pin(async move {
            if let Some(started) = self.started.take() {
                return Ok(Some(started));
            }
            if let Some(accepted) = self.accepted.take() {
                return Ok(Some(accepted));
            }
            let event = self.inner.next().await?;
            if let Some(event) = &event
                && event
                    .get("type")
                    .and_then(JsValue::as_string)
                    .is_some_and(|kind| kind.equals_str("turn_completed"))
                && let Some(result) = event.get("result")
            {
                self.live.complete_turn(result)?;
            }
            Ok(event)
        })
    }
}

struct ProductionContinuationParentNotifier {
    owner: Option<SessionOwner>,
    handle: LiveSessionHandle,
    state: Arc<LiveSessionState>,
}

impl crate::runtime::session::subagent_continuation::NativeContinuationParentNotifier
    for ProductionContinuationParentNotifier
{
    fn notify(
        &self,
        message: JsString,
    ) -> crate::runtime::session::subagent_continuation::ContinuationFuture<()> {
        let exact_handle = self.owner.as_ref().is_none_or(|owner| {
            owner
                .current_handle(&self.state.session_key)
                .is_some_and(|current| current.same_identity(&self.handle))
        });
        let authorized = exact_handle
            && self.handle.state() == crate::runtime::session::HandleState::Active
            && self
                .handle
                .snapshot()
                .get("liveIdentity")
                .and_then(JsValue::as_string)
                == Some(&self.state.identity);
        let result = if authorized {
            self.handle
                .enqueue_followup(
                    object([("type", "text".into()), ("text", JsValue::String(message))]),
                    object([]),
                )
                .map(|_| ())
                .map_err(|error| {
                    crate::runtime::session::subagent_continuation::ContinuationError::new(
                        "subagent_parent_notify_failed",
                        error.to_string(),
                    )
                })
        } else {
            Err(
                crate::runtime::session::subagent_continuation::ContinuationError::new(
                    "invalid_subagent_parent",
                    "Continuable subagent parent is no longer the exact live session.",
                ),
            )
        };
        Box::pin(async move { result })
    }
}

fn continuation_agent_config(
    snapshot: &JsValue,
    sdk_agent: Option<&JsObject>,
    cwd: &JsString,
) -> Option<JsObject> {
    let mut agent = snapshot.get("config")?.get("agent")?.as_object()?.clone();
    if let Some(sdk_agent) = sdk_agent {
        for (key, value) in sdk_agent.entries() {
            agent.insert(key.clone(), value.clone());
        }
    }
    if let Some(model) = agent.get("model").and_then(JsValue::as_object).cloned() {
        if let Some(provider) = model.get("provider") {
            agent.insert("provider".into(), provider.clone());
        }
        if let Some(model) = model.get("model") {
            agent.insert("model".into(), model.clone());
        }
    }
    agent.insert("cwd".into(), JsValue::String(cwd.clone()));
    Some(agent)
}

fn continuation_model(agent: &JsObject) -> Option<(JsString, JsString)> {
    let selected = agent
        .get("subagentModel")
        .and_then(JsValue::as_object)
        .unwrap_or(agent);
    Some((
        selected.get("provider")?.as_string()?.clone(),
        selected.get("model")?.as_string()?.clone(),
    ))
}

fn continuation_definition_ids() -> Vec<JsString> {
    ["general-purpose", "explore", "plan", "verify"]
        .into_iter()
        .map(JsString::from)
        .collect()
}

fn continuation_parent_input(agent: &JsObject, context: &SessionContext) -> JsValue {
    let mut input = JsObject::new();
    input.insert("agent".into(), JsValue::Object(agent.clone()));
    input.insert("messages".into(), JsValue::Array(Vec::new()));
    if let Some(allowed) = &context.allowed_tools {
        input.insert(
            "allowedTools".into(),
            JsValue::Array(allowed.iter().cloned().map(JsValue::String).collect()),
        );
    }
    if let Some(denied) = &context.disallowed_tools {
        input.insert(
            "disallowedTools".into(),
            JsValue::Array(denied.iter().cloned().map(JsValue::String).collect()),
        );
    }
    JsValue::Object(input)
}

fn continuation_queued_execution_input(
    template: &JsValue,
    queued_input: &JsValue,
) -> Result<JsValue, RuntimeError> {
    let mut input = template
        .as_object()
        .cloned()
        .ok_or_else(|| RuntimeError::new("Queued turn execution input is unavailable."))?;
    let kind = queued_input
        .get("type")
        .and_then(JsValue::as_string)
        .and_then(|kind| kind.to_utf8().ok())
        .unwrap_or_default();
    let content = match kind.as_str() {
        "text" => vec![object([
            ("type", "text".into()),
            (
                "text",
                queued_input
                    .get("text")
                    .cloned()
                    .ok_or_else(|| RuntimeError::new("Queued text input is missing text."))?,
            ),
        ])],
        "blocks" => match queued_input.get("content") {
            Some(JsValue::Array(content)) => content.clone(),
            _ => return Err(RuntimeError::new("Queued block input is missing content.")),
        },
        _ => return Err(RuntimeError::new("Queued turn input is invalid.")),
    };
    input.insert(
        "messages".into(),
        JsValue::Array(vec![object([
            ("role", "user".into()),
            ("content", JsValue::Array(content)),
        ])]),
    );
    Ok(JsValue::Object(input))
}

fn continuation_child_input(
    parent: &JsObject,
    descriptor: &crate::runtime::session::subagent_continuation::ContinuableSubagentDescriptor,
    child_session_id: &JsString,
) -> JsObject {
    let mut agent = parent.clone();
    agent.insert(
        "provider".into(),
        JsValue::String(descriptor.agent_provider.clone()),
    );
    agent.insert(
        "model".into(),
        JsValue::String(descriptor.agent_model.clone()),
    );
    agent.insert("isSubagent".into(), JsValue::Bool(true));
    agent.insert(
        "subagentDepth".into(),
        JsValue::Number(
            parent
                .get("subagentDepth")
                .and_then(JsValue::as_number)
                .unwrap_or(0.0)
                + 1.0,
        ),
    );
    let allowed = match descriptor
        .definition_id
        .to_utf8()
        .unwrap_or_default()
        .as_str()
    {
        "general-purpose" => vec!["*"],
        "plan" => vec!["read_file", "grep", "glob"],
        _ => vec!["read_file", "grep", "glob", "bash"],
    };
    let mut input = JsObject::new();
    input.insert("agent".into(), JsValue::Object(agent.clone()));
    input.insert(
        "allowedTools".into(),
        JsValue::Array(allowed.into_iter().map(JsValue::from).collect()),
    );
    input.insert(
        "permissionContext".into(),
        agent
            .get("permissionContext")
            .cloned()
            .unwrap_or_else(|| object([])),
    );
    input.insert(
        "interactionCapabilities".into(),
        object([("elicitationAvailable", JsValue::Bool(false))]),
    );
    input.insert("canElicit".into(), JsValue::Bool(false));
    input.insert(
        "permissionSessionId".into(),
        JsValue::String(child_session_id.clone()),
    );
    input
}

impl Drop for NativeSessionFactory {
    fn drop(&mut self) {
        let runtime = self.continuation_runtime.clone();
        let memory_registry = self.memory_registry.clone();
        let _ = std::thread::Builder::new()
            .name("native-runtime-dispose".into())
            .spawn(move || {
                if let Ok(executor) = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                {
                    let _ = executor.block_on(async move {
                        let continuation = runtime.dispose("gateway_shutdown".into()).await;
                        let memory = memory_registry.shutdown().await;
                        (continuation, memory)
                    });
                }
            })
            .and_then(|thread| {
                thread
                    .join()
                    .map_err(|_| std::io::Error::other("continuation disposer panicked"))
            });
    }
}

impl SessionFactory for NativeSessionFactory {
    fn can_recreate(&self) -> bool {
        true
    }
    fn create(
        &self,
        context: SessionContext,
        previous: Option<LiveSessionHandle>,
    ) -> BoxFuture<Result<LiveSessionHandle, RuntimeError>> {
        let project_root = context
            .project_key
            .as_ref()
            .map(|project| project.to_utf8().map(PathBuf::from))
            .transpose()
            .map(|project| project.unwrap_or_else(|| self.cwd_path.clone()));
        let cwd = self
            .live
            .cwd(&context.session_key)
            .unwrap_or_else(|| self.cwd.clone());
        let policy = self
            .policies
            .lock()
            .unwrap()
            .get(&context.session_key)
            .cloned();
        let project = context
            .project_key
            .as_ref()
            .and_then(|p| p.to_utf8().ok())
            .unwrap_or_else(|| self.cwd.to_utf8().unwrap_or_default());
        let cwd = policy
            .as_ref()
            .map(|p| JsString::from(p.cwd.to_string_lossy().as_ref()))
            .unwrap_or(cwd);
        let resources = self
            .background
            .resources(Some(&project))
            .map_err(|e| RuntimeError::new(e.message));
        let services = resources.and_then(|background| {
            crate::cron::native::NativeSessionServices::open(
                &self.pilot_home,
                &self.cwd_path,
                &project,
                &context.session_key,
                policy.as_ref().and_then(|p| p.tools.clone()),
                background,
            )
            .map(|mut services| {
                services.cron = self.cron.lock().unwrap().clone();
                Arc::new(services)
            })
            .map_err(|e| RuntimeError::new(e.to_string()))
        });
        let snapshot = load_project_snapshot(&self.pilot_home, &cwd, context.project_key.as_ref())
            .map_err(|error| RuntimeError::new(String::from_utf16_lossy(error.message.units())));
        let memory_resolver = self.memory.lock().unwrap().clone();
        let sdk_registry = self.sdk_registry.lock().unwrap().clone();
        let (builtin_plugins, builtin_skills_root) = self.plugin_resources.lock().unwrap().clone();
        let execution_error = snapshot.as_ref().err().cloned();
        let config = SidecarSessionConfig {
            limits: self.limits,
            session_key: context.session_key.clone(),
            project_key: context.project_key.clone(),
            cwd: cwd.clone(),
            snapshot: snapshot.clone().unwrap_or(JsValue::Undefined),
            services: None,
        };
        let stats = snapshot
            .as_ref()
            .ok()
            .map(|snapshot| {
                self.usage
                    .resolve_with_snapshot(context.project_key.as_ref(), snapshot)
            })
            .transpose()
            .map_err(|error| RuntimeError::new(String::from_utf16_lossy(error.message.units())));
        let spawner = self.spawner.clone();
        let adapter = self.adapter.clone();
        let live = self.live.clone();
        let home = self.pilot_home.clone();
        let cwd_path = self.cwd_path.clone();
        let history = self.history_storage.clone();
        let interactions = self.interactions.clone();
        let user_dialogs = self.user_dialogs.clone();
        let mcp_config = self.mcp_config.lock().unwrap().clone();
        let permissions = self.permissions.clone();
        let sdk = self.sdk.lock().unwrap().clone();
        let router_generations = self.router_generations.clone();
        let async_hook_bindings = self.async_hook_bindings.clone();
        let continuation_runtime = self.continuation_runtime.clone();
        let continuation_owner = self.continuation_owner.clone();
        let memory_registry = self.memory_registry.clone();
        let continuation_identity = self
            .next_continuation_identity
            .fetch_add(1, Ordering::Relaxed);
        Box::pin(async move {
            let project_root =
                project_root.map_err(|error| RuntimeError::new(error.to_string()))?;
            let project_root = crate::metadata::resolve_process_path(&cwd_path, &project_root);
            history
                .recover_replacements(&object([
                    ("sessionKey", JsValue::String(context.session_key.clone())),
                    ("projectKey", project_root.to_string_lossy().as_ref().into()),
                ]))
                .map_err(|error| RuntimeError::new(error.message))?;
            let mcp_owner = match mcp_config {
                Some(config) => {
                    crate::runtime::manager::session_mcp::SessionMcpOwner::open(
                        &config,
                        &permissions,
                        &home,
                        &project_root,
                        &context.session_key,
                    )
                    .await?
                }
                None => None,
            };
            let services = services?;
            let stats = stats?;
            let continuation_spawner = spawner.clone();
            let continuation_stats = stats.clone();
            let (session_plugins, session_skills, sdk_agent) = match sdk_registry {
                Some(sdk) => {
                    let sdk = sdk.lock().await;
                    (
                        sdk.session_plugins(&context.session_key).to_vec(),
                        sdk.session_config(&context.session_key)
                            .and_then(|config| config.get("skills"))
                            .and_then(skill_names),
                        sdk.session_config(&context.session_key)
                            .and_then(|config| config.get("settings"))
                            .and_then(|settings| settings.get("agent"))
                            .and_then(JsValue::as_object)
                            .cloned(),
                    )
                }
                None => (vec![], None, None),
            };
            let agent = snapshot
                .as_ref()
                .ok()
                .and_then(|snapshot| continuation_agent_config(snapshot, sdk_agent.as_ref(), &cwd))
                .ok_or_else(|| {
                    RuntimeError::new("Native continuation parent agent config is missing.")
                })?;
            let continuation_owner = continuation_owner.lock().unwrap().clone();
            let owner = continuation_owner
                .ok_or_else(|| RuntimeError::new("Continuation session owner is unavailable."))?;
            let (agent_provider, agent_model) = continuation_model(&agent)
                .ok_or_else(|| RuntimeError::new("Native continuation parent model is missing."))?;
            let plugin_runtime = Arc::new(crate::runtime::plugins::PluginRuntime::new(
                crate::runtime::plugins::PluginRuntimeOptions {
                    project_root: project_root.clone(),
                    pilot_home: home.clone(),
                    builtin_skills_root,
                    builtin_plugins,
                    builtin_plugins_enabled: Default::default(),
                },
            ));
            plugin_runtime
                .refresh()
                .await
                .map_err(|error| RuntimeError::new(error.message))?;
            let contribution_lease = plugin_runtime
                .acquire_session_contributions(session_plugins)
                .await
                .map_err(|error| RuntimeError::new(error.message))?;
            let native_memory = if memory_resolver.is_none() {
                match snapshot.as_ref().ok() {
                    Some(snapshot) => {
                        let fingerprint = native_memory_fingerprint(snapshot)?;
                        let project_key = project_root.to_string_lossy().into_owned();
                        memory_registry
                            .acquire(project_key, fingerprint, || {
                                stage_native_memory_for_session(
                                    &project_root,
                                    snapshot,
                                    &home.clone(),
                                    false,
                                )
                                .map_err(|error| error.message)
                            })
                            .await
                            .map_err(RuntimeError::new)?
                    }
                    None => None,
                }
            } else {
                None
            };
            let memory_timeout = snapshot
                .as_ref()
                .ok()
                .and_then(|snapshot| snapshot.get("config"))
                .and_then(|config| config.get("memory"))
                .and_then(|memory| memory.get("retrievalTimeoutMs"))
                .and_then(JsValue::as_number)
                .unwrap_or(30_000.0);
            let mut context_resources =
                crate::runtime::session::context_resources::ContextResources::from_owned_runtime_lease(
                    plugin_runtime,
                    contribution_lease,
                    project_root.to_string_lossy().into_owned(),
                );
            context_resources = match native_memory {
                Some(lease) => context_resources.with_project_memory_lease(lease),
                None => context_resources
                    .with_memory_resolver(memory_resolver)
                    .with_memory_retrieval_timeout_ms(memory_timeout),
            };
            let context_resources =
                Arc::new(context_resources.for_session_skills(session_skills.as_deref()));
            let storage = adapter.create_session_storage(SessionStorageInput {
                project_root: project_root.clone(),
                pilot_home: home.clone(),
                session_id: context.session_key.clone(),
                now: None,
            });
            let mut config = config;
            config.services = Some(services.clone());
            let mut hook_sources = match &sdk {
                Some(sdk) => Some(
                    sdk.lock()
                        .await
                        .acquire_hook_settings(&context.session_key, context.project_key.as_ref())
                        .await
                        .map_err(|error| RuntimeError::new(error.message))?,
                ),
                None => None,
            };
            let organization_policy = match &sdk {
                Some(sdk) => sdk.lock().await.options().organization_policy.clone(),
                None => None,
            };
            let router_generation = router_generations.generation(
                &JsString::from(project_root.to_string_lossy().as_ref()),
                organization_policy,
            );
            // TS ProjectSessionRuntimeBundle.ts:247-270 constructs routers
            // from the session's retained contribution lease. Do not consult
            // the live plugin generation after this point.
            let custom_routers = hook_sources
                .as_ref()
                .map(|(_, _, lease)| lease.contributions.create_custom_routers())
                .transpose()
                .map_err(|error| error.message)
                .and_then(|routers| {
                    router_generation
                        .custom_routers
                        .register_session(context.session_key.clone(), routers.unwrap_or_default())
                        .map_err(|error| String::from_utf16_lossy(error.units()))
                });
            if let Err(message) = custom_routers {
                router_generation.dispose();
                // A failed factory/registration never publishes a live session,
                // so its contribution lease must be released here.
                let mut errors = vec![RuntimeError::new(message)];
                if let Some((_, _, mut lease)) = hook_sources.take()
                    && let Err(error) = lease.release().await
                {
                    errors.push(RuntimeError::new(error.message));
                }
                return Err(if errors.len() == 1 {
                    errors.remove(0)
                } else {
                    RuntimeError {
                        message: "Failed to construct session custom routers.".into(),
                        causes: errors,
                    }
                });
            }
            let mut session = SidecarSession::new(config, spawner.clone())
                .with_owned_context_resources(context_resources.clone())
                .with_router_generation(router_generation)
                .with_user_dialogs(user_dialogs)
                .with_interactions(interactions.clone())
                .with_mcp_owner(mcp_owner.clone());
            session = session.with_hook_sources(hook_sources, async_hook_bindings);
            if let Some(sdk) = sdk {
                session = session.register_sdk_hook_handlers(sdk).await;
            }
            if let Some(stats) = stats {
                session = session.with_usage_stats(stats);
            }
            let inner: Arc<dyn LiveSession> = Arc::new(session);
            let _publication = live.publication.lock().await;
            let session = Arc::new(ContinuationParentSession::new(
                inner,
                continuation_parent_input(&agent, &context),
            ));
            let bound = crate::runtime::session::routes::host::bind(
                live.clone(),
                context.clone(),
                cwd.clone(),
                session.clone(),
                storage,
                previous,
                execution_error,
            );
            let handle = match bound {
                Ok(handle) => handle,
                Err(error) => {
                    use crate::runtime::session::LiveSession;
                    session.dispose().await?;
                    return Err(error);
                }
            };
            let state = match live.current(&context.session_key) {
                Some(state) => state,
                None => {
                    handle
                        .dispose("continuation_parent_binding_missing".into())
                        .await?;
                    return Err(RuntimeError::new(
                        "Continuation parent live binding was not published.",
                    ));
                }
            };
            let published_identity = handle
                .snapshot()
                .get("liveIdentity")
                .and_then(JsValue::as_string)
                .cloned();
            if published_identity.as_ref() != Some(&state.identity) {
                handle
                    .dispose("continuation_parent_identity_mismatch".into())
                    .await?;
                return Err(RuntimeError::new(
                    "Continuation parent live identity does not match its session handle.",
                ));
            }
            let parent = crate::runtime::session::subagent_continuation::ContinuationParent {
                session_id: context.session_key.clone(),
                identity: continuation_identity,
            };
            let child_factory = Arc::new(ProductionContinuationSessionFactory {
                parent_session_id: context.session_key.clone(),
                project_key: context.project_key.clone(),
                project_root,
                pilot_home: home.clone(),
                cwd,
                snapshot: snapshot.unwrap_or(JsValue::Undefined),
                services: Some(services),
                spawner: continuation_spawner,
                parent_live: state.clone(),
                adapter,
                context_resources,
                interactions,
                stats: continuation_stats,
                parent_agent: agent.clone(),
                continuation_runtime: continuation_runtime.clone(),
            });
            let notifier = Arc::new(ProductionContinuationParentNotifier {
                owner: Some(owner),
                handle: handle.clone(),
                state: state.clone(),
            });
            let installed = match crate::runtime::tools::builtin::subagent_continuation::install_for_parent(
                continuation_runtime,
                crate::runtime::tools::builtin::subagent_continuation::InstallSubagentContinuationRequest {
                    binding: crate::runtime::session::subagent_continuation::NativeContinuationParentBinding {
                        parent,
                        factory: child_factory,
                        notifier,
                    },
                    provider: "pilotdeck-native".into(),
                    agent_provider,
                    agent_model,
                    definitions: continuation_definition_ids(),
                },
            ) {
                Ok(installed) => installed,
                Err(error) => {
                    handle
                        .dispose("continuation_parent_install_failed".into())
                        .await?;
                    return Err(RuntimeError::new(error.message));
                }
            };
            let tool_binding = super::super::session::module_host::bind_live_continuation_tools(
                state.identity.clone(),
                installed.port(),
            );
            session.install(installed, tool_binding);
            handle.resume_queued_turns();
            Ok(handle)
        })
    }

    fn memory_registry(
        &self,
    ) -> Option<crate::runtime::session::context_resources::ProjectMemoryRegistry> {
        Some(self.memory_registry.clone())
    }
}

#[cfg(test)]
#[path = "usage_wiring_tests.rs"]
mod usage_wiring_tests;

#[cfg(test)]
mod native_memory_wiring_tests {
    use super::*;

    fn write_memory_config(root: &std::path::Path, memory_root: &std::path::Path) {
        std::fs::write(
            root.join("pilotdeck.yaml"),
            serde_json::json!({
                "schemaVersion": 1,
                "agent": { "model": "proof/base" },
                "model": {
                    "providers": {
                        "proof": {
                            "protocol": "openai",
                            "url": "http://127.0.0.1:1/v1",
                            "apiKey": "public-test-only",
                            "models": { "base": {} }
                        }
                    }
                },
                "memory": {
                    "enabled": true,
                    "provider": "edgeclaw",
                    "rootDir": memory_root
                }
            })
            .to_string(),
        )
        .unwrap();
    }

    #[tokio::test]
    async fn default_memory_stages_native_owner_but_injection_keeps_precedence() {
        let scratch = crate::scratch::Scratch::new("turn-wiring-native-memory");
        let project = scratch.path().join("project");
        let home = scratch.path().join("home");
        std::fs::create_dir_all(&project).unwrap();
        let snapshot = object([(
            "config",
            object([(
                "memory",
                object([
                    ("enabled", JsValue::Bool(true)),
                    ("provider", "edgeclaw".into()),
                    (
                        "rootDir",
                        scratch
                            .path()
                            .join("memory")
                            .to_string_lossy()
                            .as_ref()
                            .into(),
                    ),
                ]),
            )]),
        )]);
        assert!(
            stage_native_memory_for_session(&project, &snapshot, &home, true)
                .unwrap()
                .is_none(),
            "an injected resolver remains authoritative"
        );
        let owner = stage_native_memory_for_session(&project, &snapshot, &home, false)
            .unwrap()
            .expect("enabled EdgeClaw memory stages the native owner");
        assert_eq!(owner.retrieval_timeout_ms, 30_000.0);
        (owner.disposer)().await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn production_reload_recreates_session_with_new_memory_generation() {
        let scratch = crate::scratch::Scratch::new("turn-wiring-memory-reload");
        let root = scratch.path().to_path_buf();
        write_memory_config(&root, &root.join("memory-v1"));

        let provider_home = root.clone();
        let provider_cwd = root.clone();
        let composition = compose(CompositionOptions {
            pilot_home: root.clone(),
            cwd: root.to_string_lossy().as_ref().into(),
            spawner: Arc::new(BinarySidecarSpawner {
                executable: root.join("unused"),
                cwd: root.clone(),
            }),
            providers: Box::new(move |owner| {
                let mut providers = disk_providers(provider_home, provider_cwd, owner)?;
                providers.sdk = None;
                Ok(providers)
            }),
            failure_guard_enabled: false,
            abort_turn_timeout_ms: 1_000,
            server_version: "memory-reload-test".into(),
            policies: Default::default(),
            activity: Default::default(),
            background: Arc::new(
                crate::cron::runtime::BackgroundService::new(root.clone()).unwrap(),
            ),
            cron: Default::default(),
        })
        .unwrap();
        let mut config = crate::config::routes::ConfigRoutes::new(
            root.clone(),
            root.clone(),
            crate::config::Env::new(),
        )
        .unwrap();
        config.bind_host(crate::config::routes::ConfigHost::new(
            composition.owner.clone(),
        ));
        let context = SessionContext {
            session_key: "memory-reload".into(),
            project_key: None,
            channel_key: "test".into(),
            allowed_tools: None,
            disallowed_tools: None,
        };

        let original = composition
            .owner
            .get_or_create(context.clone())
            .await
            .unwrap();
        let project_key = root.to_string_lossy();
        let first_generation = composition
            .memory_registry
            .current_generation_id(&project_key)
            .await
            .expect("the production factory stages project memory");

        write_memory_config(&root, &root.join("memory-v2"));
        config
            .handle("reload_config", &object([]))
            .await
            .expect("registered config method")
            .unwrap();
        config.settle_runtime_refresh().await;
        let refreshed = composition.owner.get_or_create(context).await.unwrap();
        let second_generation = composition
            .memory_registry
            .current_generation_id(&project_key)
            .await
            .expect("the recreated production session stages replacement memory");

        assert!(!refreshed.same_identity(&original));
        assert_ne!(second_generation, first_generation);
        composition.owner.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn disk_providers_resolves_real_memory_maintenance_service_for_project() {
        let scratch = crate::scratch::Scratch::new("disk-providers-memory");
        let root = scratch.path().to_path_buf();
        write_memory_config(&root, &root.join("memory"));

        struct DummyFactory;
        impl crate::runtime::session::SessionFactory for DummyFactory {
            fn create(
                &self,
                _context: crate::runtime::session::SessionContext,
                _previous: Option<crate::runtime::session::LiveSessionHandle>,
            ) -> crate::runtime::refresh::BoxFuture<
                Result<
                    crate::runtime::session::LiveSessionHandle,
                    crate::runtime::project::RuntimeError,
                >,
            > {
                Box::pin(async move {
                    Err(crate::runtime::project::RuntimeError::new(
                        "not implemented",
                    ))
                })
            }
        }
        let owner = SessionOwner::new(crate::runtime::session::SessionOwnerOptions::new(Arc::new(
            DummyFactory,
        )));

        let providers = disk_providers(root.clone(), root.clone(), owner).unwrap();
        let maintenance = providers
            .memory_maintenance
            .expect("disk_providers provides memory_maintenance");

        let project_key = JsString::from(root.to_string_lossy().as_ref());
        // Verify schedule succeeds on real coordinator without panicking or returning early on None
        maintenance.schedule(&project_key);
        for _ in 0..10 {
            tokio::task::yield_now().await;
        }
    }
}

/// Build the disk-backed providers on the serial host. No user-home fallback is
/// used: the caller must supply the PilotHome it owns.
pub fn disk_providers(
    pilot_home: PathBuf,
    cwd: PathBuf,
    owner: SessionOwner,
) -> std::io::Result<NativeSubmitProviders> {
    use crate::runtime::{
        replacement::{ReplacementCoordinator, ReplacementStorage},
        sdk_session::{SdkSessionOptions, SdkSessionRegistry},
    };
    use crate::storage::{
        ContentAddressedWorkspaceSnapshotRecorder, TaskBudgetStore,
        resolve_workspace_snapshot_config,
    };
    let snapshots = resolve_workspace_snapshot_config(|key| std::env::var(key).ok())
        .map_err(std::io::Error::other)?
        .map(|config| Arc::new(ContentAddressedWorkspaceSnapshotRecorder::new(config)));
    let env: crate::config::Env = std::env::vars().collect();
    let builtin_plugins = crate::config::routes::bootstrap::builtin_plugins(&cwd, &env)
        .map_err(|error| std::io::Error::other(format!("{}: {}", error.code, error.message)))?;
    let builtin_skills_root = crate::config::routes::bootstrap::resource_root(
        &cwd,
        &env,
        "PILOTDECK_BUNDLED_SKILLS_DIR",
        "skills",
    );
    let resolver = Arc::new(DiskSdkResolver {
        pilot_home: pilot_home.clone(),
        cwd: cwd.clone(),
        builtin_plugins: builtin_plugins.clone(),
        builtin_skills_root: builtin_skills_root.clone(),
        plugins: Default::default(),
    });
    let mut sdk_options = SdkSessionOptions::new(&cwd, &cwd, env, resolver);
    if let Some(path) = std::env::var_os("PILOTDECK_ORGANIZATION_POLICY_PATH") {
        let value: serde_json::Value =
            serde_json::from_slice(&std::fs::read(path)?).map_err(std::io::Error::other)?;
        let mut policy = crate::runtime::sdk_session::OrganizationPolicy::default();
        policy
            .apply_execution_limits_json(&value)
            .map_err(|error| std::io::Error::other(format!("{}: {}", error.code, error.message)))?;
        sdk_options.organization_policy = Some(policy);
    }
    let task_budget = Arc::new(std::sync::Mutex::new(TaskBudgetStore::new(
        &pilot_home,
        None,
    )));
    sdk_options.task_budget = Some(super::submit_host::shared_task_budget_port(
        task_budget.clone(),
    ));
    let transcript_cwd = cwd.clone();
    let transcripts = Arc::new(crate::storage::AsyncTranscriptAdapter::new(
        Arc::new(crate::storage::JsonlTranscriptStore::new()),
        Arc::new(move |key| {
            crate::session_chat_dir(
                &key.pilot_home,
                &transcript_cwd,
                &key.project_root.to_string_lossy(),
            )
        }),
    ));
    Ok(NativeSubmitProviders {
        // Session MCP owners register browser-use runtimes in this process-wide
        // manager.  The production turn host must expose the same owner or the
        // manager_browsers route can never observe those registrations.
        manager_browsers: Some(crate::runtime::manager::BrowserManager::production().clone()),
        recover_history: None,
        replacement: Arc::new(ReplacementCoordinator::new(
            ReplacementStorage::default(),
            Arc::new(owner.clone()),
            30_000,
            None,
        )),
        snapshots,
        workspace_id: std::env::var("PILOTDECK_WORKSPACE_ID")
            .ok()
            .filter(|value| !value.is_empty())
            .map(JsString::from),
        sdk: Some(Arc::new(tokio::sync::Mutex::new(
            SdkSessionRegistry::new(sdk_options)
                .with_overrides(crate::config::session_overrides::SessionConfigOverrides::new()),
        ))),
        task_budget: Some(task_budget),
        memory: None,
        memory_maintenance: Some(Arc::new(
            crate::runtime::memory::GenerationScopedMaintenanceCoordinator::new(
                Arc::new({
                    let cwd_str = cwd.to_string_lossy().to_string();
                    let pilot_home = pilot_home.clone();
                    let cwd_path = cwd.clone();
                    let memory_reg = owner.memory_registry().unwrap_or_default();
                    move |project_key: &JsString| {
                        let project_root_str =
                            project_key.to_utf8().unwrap_or_else(|_| cwd_str.clone());
                        let project_path = std::path::PathBuf::from(&project_root_str);
                        let project_root = if project_path.is_absolute() {
                            project_path
                        } else {
                            cwd_path.join(project_path)
                        };
                        let canonical_key = project_root.to_string_lossy().to_string();

                        // 1. Prioritize reading the currently published generation from the session's memory registry!
                        if let Some(runtime) = memory_reg.resolve_published_runtime(&canonical_key)
                        {
                            return runtime;
                        }
                        if let Some(runtime) =
                            memory_reg.resolve_published_runtime(&project_root_str)
                        {
                            return runtime;
                        }

                        // 2. If not yet present in the session registry, stage it dynamically and register into the session registry!
                        let snapshot = crate::config::load_snapshot(
                            &pilot_home,
                            &project_root,
                            &std::env::vars().collect(),
                            None,
                            1.0,
                            "",
                        )
                        .ok();

                        let owner = snapshot.as_ref().and_then(|snap| {
                            stage_native_memory_for_session(&project_root, snap, &pilot_home, false)
                                .ok()
                                .flatten()
                        });

                        if let Some(owner) = owner {
                            let (generation_id, maintenance) =
                                memory_reg.stage_published_owner(&canonical_key, owner);
                            return crate::runtime::memory::ProjectMemoryMaintenanceRuntime {
                                project_root: canonical_key,
                                generation_id,
                                memory_service: Some(maintenance),
                            };
                        }

                        crate::runtime::memory::ProjectMemoryMaintenanceRuntime {
                            project_root: canonical_key,
                            generation_id: 0,
                            memory_service: None,
                        }
                    }
                }),
                Some(Arc::new(|msg, err| {
                    eprintln!("{msg} {err}");
                })),
            ),
        )),
        builtin_plugins,
        builtin_skills_root,
        uploads: Default::default(),
        pilot_home,
        transcripts,
    })
}

struct DiskSdkResolver {
    pilot_home: PathBuf,
    cwd: PathBuf,
    builtin_plugins: Vec<Arc<crate::runtime::plugins::LoadedPlugin>>,
    builtin_skills_root: Option<PathBuf>,
    plugins: std::sync::Mutex<
        std::collections::HashMap<PathBuf, Arc<crate::runtime::plugins::PluginRuntime>>,
    >,
}
impl crate::runtime::sdk_session::SdkRuntimeResolver for DiskSdkResolver {
    fn resolve(
        &self,
        project_key: Option<&JsString>,
    ) -> Result<
        crate::runtime::sdk_session::ResolvedSdkRuntime,
        crate::runtime::sdk_session::SdkSessionError,
    > {
        use crate::runtime::{
            plugins::{PluginRuntime, PluginRuntimeOptions},
            sdk_session::{ResolvedSdkRuntime, SdkSessionError},
        };
        let project = project_key
            .and_then(|key| key.to_utf8().ok())
            .filter(|value| !value.is_empty());
        let root = crate::metadata::resolve_process_path(
            &self.cwd,
            std::path::Path::new(project.as_deref().unwrap_or(".")),
        );
        let snapshot = crate::config::load_snapshot(
            &self.pilot_home,
            &self.cwd,
            &std::env::vars().collect(),
            project.as_deref(),
            1.0,
            "",
        )
        .map_err(|error| {
            SdkSessionError::new(
                "gateway_request_failed",
                String::from_utf16_lossy(error.message.units()),
            )
        })?;
        let mut plugins = self.plugins.lock().unwrap();
        let runtime = plugins
            .entry(root.clone())
            .or_insert_with(|| {
                Arc::new(PluginRuntime::new(PluginRuntimeOptions {
                    project_root: root.clone(),
                    pilot_home: self.pilot_home.clone(),
                    builtin_skills_root: self.builtin_skills_root.clone(),
                    builtin_plugins: self.builtin_plugins.clone(),
                    builtin_plugins_enabled: Default::default(),
                }))
            })
            .clone();
        Ok(ResolvedSdkRuntime {
            project_root: root,
            snapshot,
            plugin_runtime: runtime,
        })
    }
}

#[cfg(test)]
mod mcp_review_tests {
    use super::*;

    #[tokio::test]
    async fn absent_mcp_is_not_advertised_and_metadata_preserves_params() {
        let scratch = crate::scratch::Scratch::new("native-metadata");
        std::fs::write(scratch.path().join("pilotdeck.yaml"), "schemaVersion: 1\nagent: {model: proof/base}\nmodel:\n  providers:\n    proof:\n      protocol: openai\n      url: http://127.0.0.1:1/v1\n      apiKey: public-test-only\n      models: {base: {}}\n").unwrap();
        let home = scratch.path().to_path_buf();
        let providers_home = home.clone();
        let composition = compose(CompositionOptions {
            pilot_home: home.clone(),
            cwd: home.to_string_lossy().as_ref().into(),
            spawner: Arc::new(BinarySidecarSpawner {
                executable: home.join("unused"),
                cwd: home.clone(),
            }),
            providers: Box::new(move |owner| {
                let mut providers = disk_providers(providers_home.clone(), providers_home, owner)?;
                providers.sdk = None;
                Ok(providers)
            }),
            failure_guard_enabled: false,
            abort_turn_timeout_ms: 1000,
            server_version: "test".into(),
            policies: Default::default(),
            activity: Default::default(),
            background: Arc::new(crate::cron::runtime::BackgroundService::new(home).unwrap()),
            cron: Default::default(),
        })
        .unwrap();
        #[derive(Default)]
        struct Sink(std::sync::Mutex<Vec<JsValue>>);
        impl crate::runtime::connection::GatewayTextConnection for Sink {
            fn send_text(&self, message: String) {
                self.0
                    .lock()
                    .unwrap()
                    .push(crate::dispatch::parse(message.as_bytes()).unwrap());
            }
            fn close(&self, code: u16, reason: &str) {
                panic!("unexpected {code}: {reason}");
            }
        }
        let sink = Arc::new(Sink::default());
        let requests = composition
            .turn
            .connect(sink.clone(), "test-token".into(), "test".into());
        requests.receive(r#"{"type":"hello","protocolVersion":"1.2","clientName":"test","clientVersion":"1","token":"test-token"}"#.into()).await.unwrap();
        let hello = sink.0.lock().unwrap()[0].clone();
        let capabilities = hello
            .get("server")
            .or_else(|| hello.get("serverInfo"))
            .unwrap()
            .get("capabilities")
            .unwrap();
        let JsValue::Array(capabilities) = capabilities else {
            panic!("capabilities");
        };
        assert!(!capabilities.iter().any(|v| v.as_string().is_some_and(|s| {
            crate::runtime::mcp::native::METHODS
                .iter()
                .any(|m| s.equals_str(m))
        })));
        for capability in [
            "permission_list",
            "permission_decide",
            "native_transcript_archive",
            "native_transcript_archive_artifact",
            "manager_sessions",
            "manager_browsers",
        ] {
            assert!(
                capabilities
                    .iter()
                    .any(|v| v.as_string().is_some_and(|s| s.equals_str(capability)))
            );
        }
        assert!(!capabilities.iter().any(|v| {
            v.as_string()
                .is_some_and(|s| s.equals_str("sdk_session_config"))
        }));
        assert!(!capabilities.iter().any(|v| {
            v.as_string()
                .is_some_and(|s| s.equals_str("submit_turn") || s.equals_str("skill_list"))
        }));
        async fn request(
            connection: &super::super::gateway::HostedConnection,
            sink: &Sink,
            method: &str,
            params: JsValue,
        ) -> Result<JsValue, RequestError> {
            let request = object([
                ("type", "request".into()),
                ("id", "raw".into()),
                ("method", method.into()),
                ("params", params),
            ]);
            connection
                .receive(
                    pilotdeck_protocol::stringify_json(&request)
                        .unwrap()
                        .unwrap(),
                )
                .await?;
            let response = sink
                .0
                .lock()
                .unwrap()
                .iter()
                .rev()
                .find(|frame| frame.get("type") == Some(&"response".into()))
                .unwrap()
                .clone();
            if response.get("ok") == Some(&JsValue::Bool(true)) {
                return Ok(response
                    .get("result")
                    .cloned()
                    .unwrap_or(JsValue::Undefined));
            }
            Err(RequestError::new(
                response
                    .get("error")
                    .and_then(|e| e.get("message"))
                    .and_then(JsValue::as_string)
                    .cloned()
                    .unwrap(),
            ))
        }
        for value in [JsValue::Undefined, JsValue::Null] {
            let kind = if value.is_undefined() {
                "undefined"
            } else {
                "null"
            };
            let error = request(&requests, &sink, "describe_project", value)
                .await
                .unwrap_err();
            assert_eq!(
                error.message,
                JsString::from(format!(
                    "Cannot read properties of {kind} (reading 'projectKey')"
                ))
            );
        }
        let result = request(&requests, &sink, "supported_agents", JsValue::Undefined)
            .await
            .unwrap();
        assert!(matches!(result.get("agents"), Some(JsValue::Array(agents)) if !agents.is_empty()));
        let browsers = request(&requests, &sink, "manager_browsers", object([]))
            .await
            .unwrap();
        assert_eq!(browsers.get("items"), Some(&JsValue::Array(vec![])));
        let listed = request(&requests, &sink, "manager_sessions", object([]))
            .await
            .unwrap();
        assert!(matches!(listed.get("items"), Some(JsValue::Array(_))));
        let permissions = request(
            &requests,
            &sink,
            "permission_list",
            object([("sessionKey", "pending-owner".into())]),
        )
        .await
        .unwrap();
        assert_eq!(permissions.get("requests"), Some(&JsValue::Array(vec![])));

        let binding = crate::runtime::interaction::InteractionBinding::from_value(
            hello.get("interactionBinding").unwrap(),
        )
        .unwrap();
        let other_sink = Arc::new(Sink::default());
        let other =
            composition
                .turn
                .connect(other_sink.clone(), "test-token".into(), "test".into());
        other.receive(r#"{"type":"hello","protocolVersion":"1.2","clientName":"web","clientVersion":"1","token":"test-token"}"#.into()).await.unwrap();
        for method in ["permission_decide", "elicitation_respond"] {
            composition
                .interactions
                .reconnect_for_turn(&"pending-owner".into(), binding.clone())
                .unwrap();
            let delivered = Arc::new(std::sync::Mutex::new(Vec::new()));
            let received = delivered.clone();
            let entry = crate::runtime::interaction::PendingEntry {
                request_id: "pending-proof".into(),
                tool_call_id: "tool".into(),
                tool_name: "ask".into(),
                resolve: Arc::new(move |value| received.lock().unwrap().push(value)),
                reject: Arc::new(|_| panic!("must deliver, not reject")),
            };
            let pending = if method == "permission_decide" {
                composition.interactions.permission_bus().register(
                    &"pending-owner".into(),
                    entry,
                    None,
                )
            } else {
                composition.interactions.elicitation_bus().register(
                    &"pending-owner".into(),
                    entry,
                    None,
                )
            }
            .unwrap();
            let params = object([
                ("sessionKey", "pending-owner".into()),
                ("requestId", "pending-proof".into()),
                ("decision", "deny".into()),
                ("answer", object([("type", "cancelled".into())])),
            ]);
            let denied = request(&other, &other_sink, method, params.clone())
                .await
                .unwrap();
            assert_eq!(denied.get("delivered"), Some(&JsValue::Bool(false)));
            assert!(pending.active());
            assert!(delivered.lock().unwrap().is_empty());
            let wrong_owner = object([
                ("sessionKey", "other-owner".into()),
                ("requestId", "pending-proof".into()),
                ("decision", "deny".into()),
                ("answer", object([("type", "cancelled".into())])),
            ]);
            assert_eq!(
                request(&requests, &sink, method, wrong_owner)
                    .await
                    .unwrap()
                    .get("delivered"),
                Some(&JsValue::Bool(false))
            );
            assert!(pending.active());
            let accepted = request(&requests, &sink, method, params.clone())
                .await
                .unwrap();
            assert_eq!(accepted.get("delivered"), Some(&JsValue::Bool(true)));
            assert!(!pending.active());
            assert_eq!(delivered.lock().unwrap().len(), 1);
            assert_eq!(
                request(&requests, &sink, method, params)
                    .await
                    .unwrap()
                    .get("delivered"),
                Some(&JsValue::Bool(false))
            );
            assert_eq!(delivered.lock().unwrap().len(), 1);
        }
    }
}

#[cfg(test)]
mod deployment_tests {
    use super::*;

    #[cfg(unix)]
    #[tokio::test]
    async fn command_only_profile_uses_embedded_launcher_and_private_runtime_path() {
        use std::os::unix::fs::PermissionsExt;
        let scratch = crate::scratch::Scratch::new("sidecar-command-only");
        let executable = scratch.path().join("mock-sidecar");
        std::fs::write(&executable, concat!(
            "#!/usr/bin/python3\n",
            "import json, sys\n",
            "assert sys.argv[1:] == ['--stdio']\n",
            "binding = {'protocolVersion':'2.0','moduleId':'pilotdeck-agent-loop','moduleInstanceId':'command-only','connectionGeneration':'command-only-1','capabilitiesVersion':'1'}\n",
            "for method in ('hello', 'capabilities'):\n",
            "    request = json.loads(sys.stdin.readline())\n",
            "    assert request['method'] == method\n",
            "    response = {'kind':'response','messageId':'response-'+request['messageId'],'inReplyTo':request['messageId'],'ok':True,**binding}\n",
            "    if method == 'capabilities': response['payload']={'capabilitiesVersion':'1','methods':[{'name':'execute','profiles':['streaming']}]}\n",
            "    print(json.dumps(response,separators=(',',':')),flush=True)\n",
            "sys.stdin.read()\n",
        )).unwrap();
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
        let env = std::collections::HashMap::from([
            (crate::host::deployment::TRANSPORT.into(), "stdio".into()),
            (
                crate::host::deployment::SIDECAR_COMMAND.into(),
                "node".into(),
            ),
            ("PATH".into(), std::env::var("PATH").unwrap_or_default()),
        ]);
        let spawner = BinarySidecarSpawner {
            executable,
            cwd: scratch.path().to_path_buf(),
        };
        let client = spawner
            .connect_with_env(crate::host::Limits::default(), env)
            .await
            .unwrap()
            .expect("stdio profile");
        client.close().await.unwrap();
    }

    #[tokio::test]
    async fn external_stdio_sidecar_receives_the_javascript_factory_environment() {
        let scratch = crate::scratch::Scratch::new("sidecar-factory-env");
        let script = scratch.path().join("sidecar.py");
        let receipt = scratch.path().join("factory.txt");
        std::fs::write(
            &script,
            r#"import json, os, pathlib, sys
pathlib.Path(os.environ["PDRS_FACTORY_RECEIPT"]).write_text(os.environ.get("PILOTDECK_AGENT_LOOP_FACTORY", ""))
binding = {"protocolVersion":"2.0","moduleId":"pilotdeck-agent-loop","moduleInstanceId":"mock","connectionGeneration":"mock-1","capabilitiesVersion":"1"}
for method in ("hello", "capabilities"):
    request = json.loads(sys.stdin.readline())
    assert request["method"] == method
    response = {"kind":"response","messageId":"response-" + request["messageId"],"inReplyTo":request["messageId"],"ok":True,**binding}
    if method == "capabilities":
        response["payload"] = {"capabilitiesVersion":"1","methods":[{"name":"execute","profiles":["streaming"]}]}
    print(json.dumps(response, separators=(",", ":")), flush=True)
sys.stdin.read()
"#,
        )
        .unwrap();
        let env = std::collections::HashMap::from([
            (crate::host::deployment::TRANSPORT.into(), "stdio".into()),
            (
                crate::host::deployment::SIDECAR_COMMAND.into(),
                "/usr/bin/python3".into(),
            ),
            (
                crate::host::deployment::SIDECAR_PATH.into(),
                script.to_string_lossy().into_owned(),
            ),
            (
                crate::host::deployment::FACTORY.into(),
                "custom-factory.mjs".into(),
            ),
            (
                "PDRS_FACTORY_RECEIPT".into(),
                receipt.to_string_lossy().into_owned(),
            ),
        ]);
        let spawner = BinarySidecarSpawner {
            executable: scratch.path().join("unused-rust-sidecar"),
            cwd: scratch.path().to_path_buf(),
        };
        let client = spawner
            .connect_with_env(crate::host::Limits::default(), env)
            .await
            .unwrap()
            .expect("stdio profile");
        client.close().await.unwrap();
        assert_eq!(
            std::fs::read_to_string(receipt).unwrap(),
            "custom-factory.mjs"
        );
    }
}
