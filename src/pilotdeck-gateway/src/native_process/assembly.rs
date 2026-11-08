use super::{
    Options,
    families::{LspFamily, TitleFamily, TitleResource},
};
use crate::runtime::permission::effective_permission_mode;
use crate::{
    ProxyError,
    config::{Env, permission::GatewaySessionPermissionRuleSetRegistry},
    metadata::object,
    runtime::{
        async_hooks::{AsyncHookRegistry, AsyncHookTurn, TokioHookClock},
        interaction::{GatewayInteractionCoordinator, InteractionOptions},
        mcp::{
            gateway::{McpGateway, McpGatewayOptions},
            sdk::{
                PluginProjectRuntime, SdkMcpCloseSession, SdkMcpConfig, SdkMcpConfigOptions,
                SdkMcpError, SdkMcpFuture, SdkMcpProject, SdkMcpProjectResolver,
                SdkMcpSessionPlugins, SystemEnvironment,
            },
        },
        memory::{
            MemoryManagementService, MemoryProjection, MemoryProviderError, ProjectMemoryRegistry,
            gateway::MemoryGateway,
        },
        project::{
            Families, Family, ProjectResources, ProjectResourcesFactory, ProjectResourcesOptions,
            ProjectRuntimeRegistry, Resource, RuntimeError, RuntimeProfileOverrides, StageContext,
            memory_bundle::MemoryManagementResource, persistence::PersistenceResources,
            plugins::PluginResources, tools_mcp::McpProviderResource,
        },
        replacement::ReplacementCoordinator,
        sdk_session::SdkSessionProjects,
        session::{
            SessionOwner, SessionOwnerOptions,
            factory::{
                FactoryOptions, PreparedResources, PreparedSession, ProjectSessionFactory,
                RecordingPlan, SessionStorage,
            },
        },
        turn::{
            admission::{GatewaySubmitAdmission, SubmitAdmissionOptions},
            gateway::{GatewayServices, TurnGateway},
            host_factory::TurnHostFactory,
            projector::{
                GatewayAgentEventProjector, GatewayAgentEventProjectorOptions,
                GatewayToolResultArtifactStore,
            },
            publish::{FileRegistry, TurnRunRegistry},
            replay::TurnReplayOptions,
            runner::tail::TailConfig,
            submit::{TurnSubmitter, WorkspaceAdmissions},
            submit_host::{GatewaySubmitHost, NativeSubmitReplacement},
        },
    },
    storage::{FileRunRegistry, recorder::Recorder},
};
use pilotdeck_protocol::{JsObject, JsString, JsValue};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, OnceLock, Weak},
    time::{Duration, SystemTime},
};

mod cron;
mod mcp;
mod memory;
mod sdk_adapter;
mod sdk_dispatch;
pub(super) mod sdk_hooks;
pub(crate) mod snapshot;
mod steer;
mod style_reload;
mod user_dialog;

pub type Recorders = Arc<Mutex<HashMap<JsString, Weak<Recorder>>>>;

/// The cached live writer this request shares a transcript file with, if the
/// session is loaded, together with the key it is registered under. Callers
/// append *through* it instead of building a second writer: the recorder
/// numbers its appends from an in-memory sequence, so a fresh writer that
/// restores the tail and appends `sequence + 1` would either duplicate that
/// number or leave the recorder replaying a stale one. The owner key is
/// returned because `session_model_set`/`clear` must close *that* key for the
/// new selection to be read back — the request spelling can be an alias.
///
/// The lookup uses the stored-transcript identity, not the request spelling --
/// `/p` and `/p/.`, `a/b` and `a-b` name one file and therefore one writer.
///
/// #1860: lives next to [`Recorders`] because it is the map's reader; the
/// transcript fence it pairs with moved to `runtime::session::transcript_locks`
/// so the session factory can take it without a `runtime` → `native_process`
/// edge.
pub(crate) fn live_recorder(
    recorders: &Recorders,
    session_key: &JsString,
) -> Option<(JsString, Arc<Recorder>)> {
    let wanted = crate::storage::sanitize_session_id(session_key);
    let map = recorders.lock().unwrap_or_else(|error| error.into_inner());
    // Prefer the exact spelling when both it and an alias are live; either way
    // both names belong to one transcript, so any live writer is the writer.
    let mut alias = None;
    for (key, weak) in map.iter() {
        if crate::storage::sanitize_session_id(key) != wanted {
            continue;
        }
        let Some(recorder) = weak.upgrade() else {
            continue;
        };
        if key == session_key {
            return Some((key.clone(), recorder));
        }
        alias = Some((key.clone(), recorder));
    }
    alias
}

/// The controlled registry switch for collection and testing (#1369).
/// Unset keeps the production default (registry on, no receipt); "on"/"off"
/// state the switch explicitly and publish a construction receipt; every
/// other value, including the empty string, fails closed at startup.
pub(super) enum RunRegistrySwitch {
    On,
    Off,
}

pub(super) fn run_registry_switch(env: &Env) -> Result<Option<RunRegistrySwitch>, ProxyError> {
    let Some(value) = env.get("PILOTDECK_NATIVE_RUN_REGISTRY") else {
        return Ok(None);
    };
    match value.as_str() {
        "on" => Ok(Some(RunRegistrySwitch::On)),
        "off" => Ok(Some(RunRegistrySwitch::Off)),
        _ => Err(
            "native run registry switch (PILOTDECK_NATIVE_RUN_REGISTRY) must be exactly on or off."
                .into(),
        ),
    }
}

/// The controlled storage stimulus entry for collection and testing (#1370,
/// `PILOTDECK_NATIVE_STORAGE_STIMULUS`). Unset keeps the production default —
/// no control files are read and no receipts are published. A set value names
/// the control directory and publishes a construction receipt; a value that
/// is not an existing directory fails closed at startup.
pub(super) fn storage_stimulus_switch(env: &Env) -> Result<Option<PathBuf>, ProxyError> {
    let Some(value) = env.get(crate::storage::stimulus::STIMULUS_ENV) else {
        return Ok(None);
    };
    if value.trim().is_empty() {
        return Err(
            "native storage stimulus (PILOTDECK_NATIVE_STORAGE_STIMULUS) must be a control directory."
                .into(),
        );
    }
    let path = PathBuf::from(value);
    if !path.is_dir() {
        return Err(
            "native storage stimulus (PILOTDECK_NATIVE_STORAGE_STIMULUS) must name an existing directory."
                .into(),
        );
    }
    Ok(Some(path))
}

/// The controlled loop/fence stimulus entry for collection and testing
/// (#1371, `PILOTDECK_NATIVE_LOOP_STIMULUS`). Unset keeps the production
/// default — no control files are read and no receipts are published. A set
/// value names the control directory and publishes a construction receipt; a
/// value that is not an existing directory (or carries a malformed plan) fails
/// closed at startup.
pub(super) fn loop_stimulus_switch(env: &Env) -> Result<Option<PathBuf>, ProxyError> {
    let Some(value) = env.get(pilotdeck_transport::loop_stimulus::STIMULUS_ENV) else {
        return Ok(None);
    };
    pilotdeck_transport::loop_stimulus::validate_value(value)
        .map(Some)
        .map_err(Into::into)
}

/// Test/embedded entry: the production path is [`start_with_webhook`], which
/// also hands the `/feishu/webhook` resolver to the listener.
#[cfg(test)]
pub async fn start(options: Options, env: Env) -> Result<TurnGateway, ProxyError> {
    start_observed(
        options,
        env,
        Arc::new(|_, result| Box::pin(async move { result })),
        Arc::new(|error| eprintln!("native project cleanup: {error}")),
    )
    .await
}

type CleanupResult =
    std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), String>> + Send>>;
pub(super) type CleanupObserver =
    Arc<dyn Fn(&'static str, Result<(), String>) -> CleanupResult + Send + Sync>;

/// `ProjectRuntimeRegistry.ts:305-308`: when a session lease is acquired, the
/// wiring layer pushes the MCP-generated ask rules onto `lease.rules().ask`
/// through `syncSdkMcpPermissionRules`.
///
/// Returns `false` when the config was never published into the forward
/// reference. That state used to be a silent skip — the acquire path left the
/// ask array as it found it, and no ruler could tell "there was nothing to
/// sync" from "the publication regressed" (#1429). The caller reports the
/// miss instead of swallowing it.
pub(super) fn sync_acquired_ask_rules(
    config: Option<&Arc<SdkMcpConfig>>,
    rules: &crate::config::permission::RuleSetHandle,
    session_key: &JsString,
) -> bool {
    let Some(config) = config else {
        return false;
    };
    rules
        .member("ask")
        .update_array(|ask| config.sync_permission_rules(session_key, ask));
    true
}

/// Where the assembly reports a non-fatal wiring miss during a session acquire
/// (#1429). The process writes it to stderr; the `#[cfg(test)]` seam injects a
/// sink so the report itself is assertable — the miss branch needs an
/// unpublished MCP config, which the real wiring never produces.
pub(super) type AcquireMissReport = Arc<dyn Fn(&str) + Send + Sync>;

const MCP_ASK_SYNC_SKIPPED: &str = "native session: MCP ask rule sync skipped \
     (the config was not published before the session acquire)";

/// The embedding owns activation; all modes share the same runtime and cleanup.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum AssemblyMode {
    Server,
    LocalCli,
}

/// Wiring inputs. Server entries keep Default; the local embedding only
/// changes mode. Rule-set/publication/report overrides remain test-only seams.
pub(super) struct AssemblySeams {
    pub mode: AssemblyMode,
    pub permission_rule_sets: Option<Arc<GatewaySessionPermissionRuleSetRegistry>>,
    pub acquire_miss_report: Option<AcquireMissReport>,
    pub publish_mcp_config: bool,
}

impl Default for AssemblySeams {
    fn default() -> Self {
        Self {
            mode: AssemblyMode::Server,
            permission_rule_sets: None,
            acquire_miss_report: None,
            publish_mcp_config: true,
        }
    }
}

// An observer runs only after the actual production disposal. Tests can inject
// a failure or a deterministic completion gate without substituting resources.
#[cfg(test)]
pub(super) async fn start_observed(
    options: Options,
    env: Env,
    cleanup_observer: CleanupObserver,
    cleanup_diagnostic: Arc<dyn Fn(&str) + Send + Sync>,
) -> Result<TurnGateway, ProxyError> {
    start_observed_with_upload_cleanup(
        options,
        env,
        crate::upload::CLEANUP_INTERVAL,
        cleanup_observer,
        cleanup_diagnostic,
    )
    .await
}

#[cfg(test)]
pub(super) async fn start_observed_with_upload_cleanup(
    options: Options,
    env: Env,
    upload_cleanup_interval: Duration,
    cleanup_observer: CleanupObserver,
    cleanup_diagnostic: Arc<dyn Fn(&str) + Send + Sync>,
) -> Result<TurnGateway, ProxyError> {
    start_assembly(
        options,
        env,
        upload_cleanup_interval,
        cleanup_observer,
        cleanup_diagnostic,
        None,
        None,
        AssemblySeams::default(),
    )
    .await
}

/// Production entry: the host plus the `/feishu/webhook` resolver the native
/// listener mounts (`activeFeishu`, resolved per request so a stop or reload
/// changes the answer), and the #1353 switch handles the embedding's machine
/// drives (the cron stop handle, the channel assembly).
pub(super) async fn start_with_webhook(
    options: Options,
    env: Env,
) -> Result<
    (
        TurnGateway,
        crate::native_http::WebhookResolver,
        super::switch_wiring::AssemblySwitchHandles,
    ),
    ProxyError,
> {
    let slot = Arc::new(std::sync::OnceLock::new());
    let switch_slots = super::switch_wiring::AssemblySwitchSlots::default();
    let host = start_assembly(
        options,
        env,
        crate::upload::CLEANUP_INTERVAL,
        Arc::new(|_, result| Box::pin(async move { result })),
        Arc::new(|error| eprintln!("native project cleanup: {error}")),
        Some(slot.clone()),
        Some(switch_slots.clone()),
        AssemblySeams::default(),
    )
    .await?;
    let slot = slot.get().cloned().ok_or_else(|| {
        ProxyError::from("feishu webhook slot was not constructed during assembly")
    })?;
    // The cron slot is filled by the builder on the gateway thread when a
    // `cron:` section exists; `start_assembly` returning means the builder
    // ran, so reading it here is deterministic (the feishu slot above reads
    // the same way).
    Ok((
        host,
        crate::channel::feishu::FeishuWebhookSlot::resolver(&slot),
        super::switch_wiring::AssemblySwitchHandles {
            cron: switch_slots.cron.get().cloned(),
            channels: switch_slots.channels.get().cloned().ok_or_else(|| {
                ProxyError::from("channel assembly slot was not constructed during assembly")
            })?,
        },
    ))
}

/// #1429: the session permission rule set registry is normally created inside
/// the assembly, which leaves the acquire-time MCP ask sync unobservable from a
/// test. Supplying the registry keeps the assertion on the production entry
/// point (real routes, real prepare closure) instead of a copy of it. The miss
/// sink and the publication switch exist only for tests: production always
/// publishes the MCP config and writes a miss to stderr.
#[cfg(test)]
#[allow(clippy::too_many_arguments)]
pub(super) async fn start_observed_with_permission_rule_sets(
    options: Options,
    env: Env,
    permission_rule_sets: Arc<GatewaySessionPermissionRuleSetRegistry>,
    miss_report: AcquireMissReport,
    publish_mcp_config: bool,
    cleanup_observer: CleanupObserver,
    cleanup_diagnostic: Arc<dyn Fn(&str) + Send + Sync>,
) -> Result<TurnGateway, ProxyError> {
    start_assembly(
        options,
        env,
        crate::upload::CLEANUP_INTERVAL,
        cleanup_observer,
        cleanup_diagnostic,
        None,
        None,
        AssemblySeams {
            mode: AssemblyMode::Server,
            permission_rule_sets: Some(permission_rule_sets),
            acquire_miss_report: Some(miss_report),
            publish_mcp_config,
        },
    )
    .await
}

/// The process-independent CLI host uses the same services and cleanup, but
/// does not activate the background services owned by a gateway server.
pub(super) async fn start_local(options: Options, env: Env) -> Result<TurnGateway, ProxyError> {
    start_assembly(
        options,
        env,
        crate::upload::CLEANUP_INTERVAL,
        Arc::new(|_, result| Box::pin(async move { result })),
        Arc::new(|error| eprintln!("native project cleanup: {error}")),
        None,
        None,
        AssemblySeams {
            mode: AssemblyMode::LocalCli,
            ..Default::default()
        },
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn start_assembly(
    options: Options,
    env: Env,
    upload_cleanup_interval: Duration,
    cleanup_observer: CleanupObserver,
    cleanup_diagnostic: Arc<dyn Fn(&str) + Send + Sync>,
    webhook_slot: Option<Arc<std::sync::OnceLock<Arc<crate::channel::feishu::FeishuWebhookSlot>>>>,
    switch_slots: Option<super::switch_wiring::AssemblySwitchSlots>,
    seams: AssemblySeams,
) -> Result<TurnGateway, ProxyError> {
    let AssemblySeams {
        mode,
        permission_rule_sets,
        acquire_miss_report,
        publish_mcp_config,
    } = seams;
    let registry_switch = run_registry_switch(&env)?;
    let stimulus_switch = storage_stimulus_switch(&env)?;
    if let Some(dir) = &stimulus_switch {
        // The #1368/#1370 storage stimulus boundary witness: the controlled
        // entry states its resolved directory at construction, before any turn
        // is driven. The receipt format follows the #1369 construction style.
        eprintln!("native storage stimulus on {}", dir.display());
    }
    let loop_switch = loop_stimulus_switch(&env)?;
    // #1371: only a resolved *plan* replaces the turn. An armed directory
    // without a plan is inert, so it must keep the production behaviour byte
    // for byte, automatic title request included. The plan is resolved here,
    // at gateway startup; the sidecar resolves the same file when it starts,
    // so the collector must write `loop.json` before launching the gateway
    // (see the `loop_stimulus` module docs) or the two processes can disagree.
    let loop_planned = pilotdeck_transport::loop_stimulus::LoopStimulus::global()
        .is_some_and(|stimulus| stimulus.is_planned());
    if let Some(dir) = &loop_switch {
        // #1371: the controlled loop/fence entry states its directory before
        // any turn is driven, in the same construction style as #1370.
        eprintln!("native loop stimulus on {}", dir.display());
    }
    if let Some(switch) = &registry_switch {
        // The #1368 registry boundary witness: the switch itself states its
        // resolved state at construction, before any turn is driven.
        let state = match switch {
            RunRegistrySwitch::On => "on",
            RunRegistrySwitch::Off => "off",
        };
        eprintln!("native run registry {state}");
    }
    // Management can inspect existing skills; turn admission still checks that
    // unsupported skill prompt/tool composition is not silently omitted.
    validate_profile_inner(&options, &env, true).await?;
    let prepare_deployment = super::deployment::resolve(&options, &env)?;
    let registration_permit = super::methods::session_dir::session_io_permit()
        .await
        .map_err(|error| String::from_utf16_lossy(error.message.units()))?;
    let registration_options = options.clone();
    tokio::task::spawn_blocking(move || {
        let _permit = registration_permit;
        register_project(&registration_options)
    })
    .await
    .map_err(|error| format!("Native project registration task failed: {error}"))??;
    let project = project_registry(&options, &env);
    let snapshot_enabled = snapshot::snapshot_enabled(&options, &project).await?;
    let snapshot_recorder =
        snapshot::snapshot_recorder(&options, &project, snapshot_enabled).await?;
    let memory = memory::build_memory(options.clone(), project.clone()).await?;
    let recorders: Recorders = Arc::default();
    let permission_rule_sets = permission_rule_sets.unwrap_or_default();
    let sdk_handlers = Arc::new(std::sync::Mutex::new(
        crate::runtime::sdk_session::SessionHandlerRegistry::new(),
    ));
    let delivered_handlers = sdk_handlers.clone();
    let interactions = Arc::new(GatewayInteractionCoordinator::new(InteractionOptions {
        permission_grants: Some(permission_rule_sets.clone()),
        on_elicitation_delivered: Some(Arc::new(move |session, request| {
            let handler = delivered_handlers
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .session_hook_handler(session)
                .cloned();
            if let Some(handler) = handler {
                handler(
                    crate::runtime::sdk_session::PilotDeckHookEvent::ElicitationResult,
                    &object([
                        ("requestId", JsValue::String(request.clone())),
                        ("delivered", JsValue::Bool(true)),
                    ]),
                );
            }
        })),
        ..InteractionOptions::default()
    }));
    let user_dialog_bus = user_dialog::build_user_dialog_bus();
    // #1864: the per-gateway session selection store. One instance serves
    // both writers (the three selector methods on `Requests`) and the reader
    // (the prepare closure composing each session's agent config).
    let session_overrides: Arc<super::SessionOverrides> = Arc::default();
    let prepare_overrides = session_overrides.clone();
    let sdk_projects = SdkSessionProjects::shared();
    let sdk_options = crate::runtime::sdk_session::SdkSessionOptions::new(
        options.project.clone(),
        options.project.clone(),
        env.clone(),
        Arc::new(sdk_adapter::NativeSdkResolver {
            project: project.clone(),
        }),
    )
    .with_homedir(options.home.clone())
    .with_projects(sdk_projects.clone());
    let prepare_mcp_config: Arc<std::sync::OnceLock<Arc<SdkMcpConfig>>> = Arc::default();
    let sdk_registry = crate::runtime::sdk_session::SdkSessionRegistry::new(sdk_options)
        .with_shared_handlers(sdk_handlers.clone());
    let sdk_async_hooks = Arc::new(std::sync::OnceLock::new());
    let sdk_port = Arc::new(sdk_adapter::NativeSdkPort {
        mcp_config: prepare_mcp_config.clone(),
        handlers: sdk_handlers.clone(),
        async_hooks: sdk_async_hooks.clone(),
        registry: tokio::sync::Mutex::new(sdk_registry),
    });
    let prepare_sdk = sdk_port.clone();
    let fork_registry = super::agent::NativeForkRegistry::new();
    let prepare_forks = fork_registry.clone();
    let session_events = Arc::new(super::session_events::NativeSessionEvents::default());
    let prepare_session_events = session_events.clone();
    let storage_forks = fork_registry.clone();
    let prepare_project = project.clone();
    let prepare_options = options.clone();
    let prepare_env = env.clone();
    let prepare_recorders = recorders.clone();
    let prepare_interactions = interactions.clone();
    let prepare_permission_rule_sets = permission_rule_sets.clone();
    // Forward reference so the prepare closure can call sync_permission_rules
    // after lease acquire (mirrors ProjectRuntimeRegistry.ts:305-308).
    // #1429: report at most once per assembly — a per-acquire stderr write would
    // be unbounded if the publication regressed for good.
    let acquire_miss_report: AcquireMissReport =
        acquire_miss_report.unwrap_or_else(|| Arc::new(|message: &str| eprintln!("{message}")));
    let prepare_acquire_miss_report = acquire_miss_report.clone();
    let prepare_acquire_miss_reported: Arc<std::sync::OnceLock<()>> = Arc::default();
    // #1433: the prepare closure's MCP composition diagnostics; production
    // logs to stderr like the factory's session diagnostic.
    let prepare_mcp_diagnostic: Option<crate::runtime::mcp::registry::McpDiagnosticSink> = Some(
        Arc::new(|message: String, error: Option<RuntimeError>| match error {
            Some(error) => eprintln!("native session MCP: {message}: {error}"),
            None => eprintln!("native session MCP: {message}"),
        }),
    );
    // The `#[cfg(test)]` seam passes `false` to drive the published-miss branch;
    // production always publishes into the handle the prepare closure reads.
    let build_mcp_config = if publish_mcp_config {
        prepare_mcp_config.clone()
    } else {
        Arc::new(std::sync::OnceLock::new())
    };
    let storage_project = project.clone();
    let storage_options = options.clone();
    // #897: the composition-time budget preflight resolves the same directory
    // the per-turn ports use (`SessionTools::turn`), so the two cannot drift.
    let budget_project = options.project.clone();
    let publication_forks = fork_registry.clone();
    let factory = Arc::new(ProjectSessionFactory::new(FactoryOptions {
        writer_busy_wait_timeout: options.drain_budget,
        prepare: Arc::new(move |context, session_mcp_runtimes| {
            let project = prepare_project.clone();
            let forks = prepare_forks.clone();
            let session_events = prepare_session_events.clone();
            let options = prepare_options.clone();
            let env = prepare_env.clone();
            let deployment = prepare_deployment.clone();
            let recorders = prepare_recorders.clone();
            let interactions = prepare_interactions.clone();
            let permission_rule_sets = prepare_permission_rule_sets.clone();
            let mcp_config_handle = prepare_mcp_config.clone();
            let acquire_miss_report = prepare_acquire_miss_report.clone();
            let acquire_miss_reported = prepare_acquire_miss_reported.clone();
            let mcp_diagnostic = prepare_mcp_diagnostic.clone();
            let session_overrides = prepare_overrides.clone();
            let sdk = prepare_sdk.clone();
            Box::pin(async move {
                let fork_scope = forks.scope(&context);
                let lease = match &fork_scope {
                    Some(scope) => scope
                        .generation
                        .upgrade()
                        .ok_or_else(|| {
                            RuntimeError::new("Parent fork execution world has been released.")
                        })?
                        .fork()?,
                    None => project.acquire(None)?,
                };
                let resources = lease
                    .runtime()
                    .resources()
                    .get::<ProjectResources>()
                    .ok_or_else(|| RuntimeError::new("Native project resources are missing."))?;
                let staged = resources.context();
                let pinned_steer = staged.resource(Resource::InteractionSteer).and_then(|resource| {
                    resource.as_any().downcast_ref::<crate::runtime::project::interaction_steer::InteractionSteer>().cloned()
                });
                let title = staged
                    .owner(Family::SessionTitleProvider)
                    .ok_or_else(|| RuntimeError::new("Native title provider is missing."))?;
                let title = title
                    .as_any()
                    .downcast_ref::<TitleResource>()
                    .ok_or_else(|| RuntimeError::new("Native title provider has the wrong type."))?
                    .0
                    .clone();
                let lsp = staged
                    .owner(Family::LspService)
                    .ok_or_else(|| RuntimeError::new("Native LSP service is missing."))?;
                let lsp = Arc::new(
                    lsp.as_any()
                        .downcast_ref::<super::families::DefaultLsp>()
                        .ok_or_else(|| RuntimeError::new("Native LSP service has the wrong type."))?
                        .bind()?,
                );
                #[cfg(unix)]
                let (background_tasks, native_sandbox) = {
                    let resource = staged
                        .resource(Resource::ExecutionWorld)
                        .ok_or_else(|| RuntimeError::new("Native execution world is missing."))?;
                    let world = resource
                        .as_any()
                        .downcast_ref::<crate::runtime::background::BackgroundExecutionWorld>()
                        .ok_or_else(|| {
                            RuntimeError::new("Native execution world has the wrong type.")
                        })?;
                    (world.resources.tasks.clone(), world.sandbox.clone())
                };
                let snapshot = staged.snapshot();
                let router_resource = staged
                    .resource(Resource::Router)
                    .ok_or_else(|| RuntimeError::new("Native router is missing."))?;
                let router = router_resource
                    .as_any()
                    .downcast_ref::<crate::runtime::project::model::RouterResources>()
                    .ok_or_else(|| RuntimeError::new("Native router has the wrong type."))?;
                let hook_proxy = snapshot
                    .get("config")
                    .and_then(|v| v.get("proxy"))
                    .and_then(crate::runtime::model::http::ProxySettings::from_config);
                let native_web = super::web::NativeWebTools::new(
                    router.router.clone(),
                    router.model_config.clone(),
                    &snapshot,
                    options.project.to_string_lossy().into_owned().into(),
                    &env,
                );
                let agent = snapshot
                    .get("config")
                    .and_then(|v| v.get("agent"))
                    .cloned()
                    .unwrap_or(JsValue::Undefined);
                let mut config = agent.as_object().cloned().unwrap_or_default();
                if let Some(sdk) = sdk.prompt_config(&context.session_key).await {
                    sdk_adapter::apply_prompt_config(&mut config, &sdk);
                }

                let selected = agent.get("model").cloned().unwrap_or(JsValue::Undefined);
                for key in ["provider", "model"] {
                    config.insert(
                        key.into(),
                        selected.get(key).cloned().unwrap_or(JsValue::Undefined),
                    );
                }
                let model = staged
                    .resource(Resource::Model)
                    .ok_or_else(|| RuntimeError::new("Native model resource is missing."))?;
                let model = model
                    .as_any()
                    .downcast_ref::<crate::runtime::project::model::ModelResources>()
                    .ok_or_else(|| {
                        RuntimeError::new("Native model resource has the wrong type.")
                    })?;
                use crate::runtime::model::router::RouterModelInvocation;
                let capabilities = selected
                    .get("provider")
                    .and_then(JsValue::as_string)
                    .zip(selected.get("model").and_then(JsValue::as_string))
                    .and_then(|(p, m)| model.providers.get_capabilities(p, m).ok())
                    .unwrap_or(JsValue::Undefined);
                for key in ["maxContextTokens", "maxOutputTokens"] {
                    if config
                        .get(key)
                        .is_none_or(|v| matches!(v, JsValue::Undefined | JsValue::Null))
                    {
                        config.insert(
                            key.into(),
                            capabilities.get(key).cloned().unwrap_or(JsValue::Undefined),
                        );
                    }
                }
                if let Some(limit) = env
                    .get("PILOTDECK_MAX_OUTPUT_TOKENS")
                    .map(|v| crate::js_value::string_to_number(v))
                    .filter(|v| v.is_finite() && *v > 0.0 && v.fract() == 0.0)
                {
                    config.insert("maxOutputTokens".into(), JsValue::Number(limit));
                }
                super::agent::apply_subagent_settings(&mut config);
                config.insert("jsonSelfCorrect".into(), JsValue::Bool(true));
                config.insert(
                    "cwd".into(),
                    JsValue::String(options.project.to_string_lossy().into_owned().into()),
                );
                // #1864: compose the session selections the three selector
                // methods stored — the native counterpart of
                // SessionAgentConfigBundle.compose over the registry maps.
                // The permission-mode source for the per-turn override (b)
                // reads the same store live, mirroring
                // InProcessGateway.ts:1443.
                let stored_permission_mode =
                    session_overrides.permission_mode(&context.session_key);
                let stored_thinking = session_overrides.thinking(&context.session_key);
                let stored_style_content =
                    match session_overrides.output_style(&context.session_key) {
                        Some(name) => output_style_content(&staged, &name).await,
                        None => None,
                    };
                apply_session_overrides(
                    &mut config,
                    stored_permission_mode.as_ref(),
                    stored_thinking.as_ref(),
                    stored_style_content.as_deref(),
                );
                // This ceiling comes from the actual project generation and is
                // inherited even when a child receives a fresh session key.
                let project_skills_disabled = snapshot
                    .get("config")
                    .and_then(|v| v.get("modules"))
                    .and_then(|v| v.get("skills"))
                    .and_then(|v| v.get("enabled"))
                    == Some(&JsValue::Bool(false));
                if let Some(scope) = &fork_scope {
                    config = scope.compose();
                    // A child route never inherits the parent's token caps.
                    let caps = config
                        .get("provider")
                        .and_then(JsValue::as_string)
                        .zip(config.get("model").and_then(JsValue::as_string))
                        .and_then(|(p, m)| model.providers.get_capabilities(p, m).ok())
                        .unwrap_or(JsValue::Undefined);
                    for key in ["maxContextTokens", "maxOutputTokens"] {
                        if config
                            .get(key)
                            .is_none_or(|value| matches!(value, JsValue::Undefined | JsValue::Null))
                        {
                            config.insert(
                                key.into(),
                                caps.get(key).cloned().unwrap_or(JsValue::Undefined),
                            );
                        }
                    }
                }
                let skills_disabled = project_skills_disabled
                    || config.get("nativeSkillsDisabled") == Some(&JsValue::Bool(true));
                config.insert(
                    "nativeSkillsDisabled".into(),
                    JsValue::Bool(skills_disabled),
                );
                // Explicit host output budgets also cap a selected child route.
                if let Some(limit) = env
                    .get("PILOTDECK_MAX_OUTPUT_TOKENS")
                    .map(|v| crate::js_value::string_to_number(v))
                    .filter(|v| v.is_finite() && *v > 0.0 && v.fract() == 0.0)
                {
                    config.insert("maxOutputTokens".into(), JsValue::Number(limit));
                }
                // Both native read_skill and prompt assembly consume this one
                // immutable plugin-generation catalog. Reload may retire it,
                // but the session lease keeps its selected bytes alive.
                let plugin_resource = staged
                    .resource(Resource::PluginRuntime)
                    .ok_or_else(|| RuntimeError::new("Native skill runtime is missing."))?;
                let plugin_runtime = plugin_resource
                    .as_any()
                    .downcast_ref::<PluginResources>()
                    .ok_or_else(|| RuntimeError::new("Native skill runtime has the wrong type."))?
                    .runtime
                    .clone();
                plugin_runtime
                    .refresh()
                    .await
                    .map_err(|error| RuntimeError::new(error.to_string()))?;
                let mut skill_lease = plugin_runtime
                    .acquire_session_contributions(vec![])
                    .await
                    .map_err(|error| RuntimeError::new(error.to_string()))?;
                let plugin_hooks = skill_lease.contributions.hooks.clone();
                let native_skills = if skills_disabled {
                    super::skills::NativeSkills::disabled()
                } else if let Some(scope) = &fork_scope {
                    scope.skills.clone()
                } else {
                    super::skills::NativeSkills::from_contributions(
                        &skill_lease.contributions.skills,
                    )
                };
                let native_skills = native_skills.select(config.get("nativeSkillSelection"))?;
                let context_storage = staged
                    .resource(Resource::InstructionStorage)
                    .ok_or_else(|| RuntimeError::new("Native context storage is missing."))?;
                let context_storage = context_storage.as_any()
                    .downcast_ref::<crate::runtime::project::context_storage::ContextStorageResources>()
                    .ok_or_else(|| RuntimeError::new("Native context storage has the wrong type."))?;
                let native_context = Arc::new(
                    context_storage
                        .create_session_with_options(
                            super::tools::session_tool_results_dir(
                                &options.project,
                                &context.session_key,
                            ),
                            crate::runtime::project::context_storage::SessionContextOptions {
                                extension: Some(Arc::new(native_skills.clone())),
                                memory_disabled: config.get("nativeMemoryDisabled")
                                    == Some(&JsValue::Bool(true)),
                                ..Default::default()
                            },
                        )?
                        .context_runtime,
                );
                let mut retained = PreparedResources::default();
                let fork_owner = super::agent::NativeForkOwner::new(lease);
                let pinned_generation = if fork_scope.is_some() {
                    fork_owner.generation().map(|lease| Arc::downgrade(&lease))
                } else {
                    None
                };
                let release_owner = fork_owner.clone();
                retained.push(move || release_owner.release_generation());
                retained.push(move || {
                    Box::pin(async move {
                        skill_lease
                            .release()
                            .await
                            .map_err(|error| RuntimeError::new(error.to_string()))
                    })
                });
                let permission_lease = permission_rule_sets.acquire(&context.session_key, None);
                let native_permission_rules = fork_scope
                    .as_ref()
                    .map(|scope| scope.permission_rules.clone())
                    .unwrap_or_else(|| permission_lease.rules());
                // Sync MCP ask rules on acquire, mirroring
                // ProjectRuntimeRegistry.ts:305-308 where the wiring layer
                // calls `lease.rules().ask` via syncSdkMcpPermissionRules.
                if !sync_acquired_ask_rules(
                    mcp_config_handle.get(),
                    &permission_lease.rules(),
                    &context.session_key,
                ) {
                    // #1429: this used to be a silent skip, so no ruler could
                    // tell a broken publication from an empty ask array.
                    // `build_mcp_gateway` publishes the config before the host
                    // serves any request, so reaching here means the startup
                    // wiring regressed; say it out loud (once per assembly).
                    if acquire_miss_reported.set(()).is_ok() {
                        acquire_miss_report(MCP_ASK_SYNC_SKIPPED);
                    }
                }
                retained.push(move || {
                    Box::pin(async move {
                        permission_lease.release();
                        Ok(())
                    })
                });
                // #1433: compose the session's MCP tools
                // (`SessionMcpRuntimeBundle.compose`) from the enabled session
                // server table — the production read face of
                // `SdkMcpConfig::session_mcp_servers`. Shared servers lease the
                // project runtime; per-session stdio servers start under the
                // factory's per-session registry. An unpublished config keeps
                // the empty composition: the ask-rule sync above already
                // reported that wiring miss for this assembly (#1429).
                let native_observer = config.get("nativeObserver") == Some(&JsValue::Bool(true));
                let (mut session_mcp, session_mcp_release) = match mcp_config_handle.get().filter(|_| !native_observer) {
                    Some(mcp_config) => {
                        let provider = staged
                            .owner(Family::McpProvider)
                            .ok_or_else(|| RuntimeError::new("Native MCP provider is missing."))?;
                        let provider = provider
                            .as_any()
                            .downcast_ref::<McpProviderResource>()
                            .ok_or_else(|| {
                                RuntimeError::new("Native MCP provider has the wrong type.")
                            })?
                            .provider()
                            .clone();
                        provider.preload_native_config().await?;
                        // #1433 fix-r1 (review P2#4): read the snapshot's
                        // already-normalized key — `config/snapshot.rs`
                        // normalizes it to a finite number with a floor of 1
                        // (the TS `numberField(..., 5)` + `Math.max(1, …)`),
                        // so re-clamping here would be a second
                        // normalization, and the old `as usize` cast
                        // truncated a fractional cap (TS `size >= 2.5` still
                        // admits the third registration).
                        let max_per_session_instances = snapshot
                            .get("config")
                            .and_then(|config| config.get("gateway"))
                            .and_then(|gateway| gateway.get("maxPerSessionMcpInstances"))
                            .and_then(JsValue::as_number)
                            .unwrap_or(5.0);
                        super::tools::session_mcp_tools(super::tools::SessionMcpToolsOptions {
                            mcp_config,
                            provider: &provider,
                            per_session_runtimes: &session_mcp_runtimes,
                            session_key: &context.session_key,
                            max_per_session_instances,
                            on_diagnostic: mcp_diagnostic.clone(),
                        })
                        .await?
                    }
                    None => (
                        super::tools::SessionMcpTools::empty(),
                        super::tools::SessionMcpRelease::default(),
                    ),
                };
                retained.push(move || Box::pin(async move { session_mcp_release.release().await }));
                let mut fork_mcp_names = Vec::new();
                if !native_observer
                    && let Some(servers) = config
                    .get("nativeForkMcpServers")
                    .and_then(JsValue::as_object)
                    .filter(|servers| !servers.entries().is_empty())
                {
                    let provider = staged
                        .owner(Family::McpProvider)
                        .and_then(|owner| {
                            owner
                                .as_any()
                                .downcast_ref::<McpProviderResource>()
                                .map(|value| value.provider().clone())
                        })
                        .ok_or_else(|| {
                            RuntimeError::new("Native child MCP provider is missing.")
                        })?;
                    let definition = config
                        .get("nativeForkMcpDefinition")
                        .and_then(JsValue::as_object)
                        .ok_or_else(|| {
                            RuntimeError::new("Native child MCP definition is missing.")
                        })?;
                    let (child_tools, release) = super::tools::fork_mcp_tools(
                        &provider,
                        &session_mcp_runtimes,
                        &context.session_key,
                        servers,
                        definition,
                    )
                    .await?;
                    fork_mcp_names
                        .extend(child_tools.iter().map(|tool| tool.spec().wire_name.clone()));
                    session_mcp.append_fork(child_tools);
                    retained.push(move || Box::pin(async move { release.release().await }));
                }
                let plan_files = Arc::new(
                    super::plan_todo::NativePlanFiles::prepare(options.project.clone())
                        .await
                        .map_err(|error| RuntimeError::new(error.to_string()))?,
                );
                let tail = TailConfig {
                    cwd: Some(options.project.clone()),
                    // #1371: the controlled loop emits no assistant text, so
                    // the automatic title request is a stimulus difference
                    // (RESULT.md TEXT row). An armed loop *with a plan*
                    // removes that side effect instead of leaving a
                    // non-comparable extra provider request in the recorded
                    // cells; an inert entry keeps the production request.
                    auto_generate_title: !loop_planned && fork_scope.is_none(),
                    title_provider: (!loop_planned && fork_scope.is_none()).then_some(title),
                    ..Default::default()
                };
                let session = context.session_key;
                let session_emit = {
                    let owner = Arc::downgrade(&fork_owner);
                    session_events.bind(
                        session.clone(),
                        Arc::new(move || owner.upgrade().is_some_and(|owner| owner.is_open())),
                    )
                };
                let closing_owner = fork_owner.clone();
                retained.push(move || closing_owner.drain());
                Ok(PreparedSession {
                    resources: retained,
                    tail,
                    host: Box::new(move |binding| {
                        if let Some(scope) = &fork_scope {
                            scope.seed_checkpoint(&binding.checkpoint)?;
                        }
                        let resume_sdk=Arc::downgrade(&sdk);
                        let resume_session=session.clone();
                        let resume_emit=session_emit.clone();
                        forks.bind_session(&binding, &fork_owner, Arc::new(move |seed| {
                            let sdk=resume_sdk.clone();
                            let session=resume_session.clone();
                            let emit=resume_emit.clone();
                            Box::pin(async move {
                                let sdk=sdk.upgrade().ok_or_else(||RuntimeError::new("Session SDK owner is closed."))?;
                                let config=sdk.prompt_config(&session).await;
                                let sdk_hooks=crate::runtime::turn::admission::SubmitSdkSessionConfig::hooks(sdk.as_ref(),&session)
                                    .await.map_err(|error|RuntimeError::new(error.message))?;
                                let sdk_permission_mode=sdk.permission_mode(&session).await;
                                let can_prompt=Some(seed.as_ref().map(|authority|authority.can_prompt.unwrap_or(true)).unwrap_or(false));
                                let policy_can_prompt=(seed.as_ref().is_some_and(|authority|authority.policy_can_prompt==Some(false))
                                    || config.as_ref().and_then(|config|config.get("managedPermissions")).and_then(|permissions|permissions.get("canPrompt"))==Some(&JsValue::Bool(false))).then_some(false);
                                Ok(Some(crate::runtime::turn::interaction::TurnInteractionAuthority{
                                    permission_mode:None,base_permission_mode:None,can_prompt,policy_can_prompt,
                                    sdk_permission_mode,sdk_hooks,emit,
                                }))
                            })
                        }))?;
                        recorders
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .insert(session.clone(), Arc::downgrade(&binding.recorder));
                        // #1864 (b): one source closure feeds both the session
                        // tools and the live adapter — the same store and key
                        // the prepare closure read back.
                        let permission_source = {
                            let session_overrides = session_overrides.clone();
                            let session = session.clone();
                            let child_mode = fork_scope
                                .as_ref()
                                .map(|scope| scope.permission_mode.clone());
                            Arc::new(move || {
                                child_mode
                                    .clone()
                                    .or_else(|| session_overrides.permission_mode(&session))
                            })
                        };
                        let ports = super::tools::SessionTools::new(
                            &options,
                            &env,
                            binding,
                            config.clone(),
                            interactions,
                            lsp,
                            session_mcp,
                        )?
                        .with_session_emitter(session_emit)
                        .with_pinned_steer(pinned_steer)
                        .with_permission_mode_source(permission_source.clone())
                        .with_permission_rules(native_permission_rules)
                        .with_web(native_web)
                        .with_plugin_hooks(plugin_hooks)
                        .with_hook_proxy(hook_proxy)
                        .with_plan_files(plan_files)
                        .with_skills(native_skills, native_context)
                        .with_fork_mcp_names(fork_mcp_names)
                        .with_forks(forks, fork_scope, fork_owner);
                        #[cfg(unix)]
                        let ports = ports
                            .with_background_tasks(background_tasks)
                            .with_native_sandbox(native_sandbox);
                        Ok(TurnHostFactory::new_with_authority(
                            project,
                            deployment,
                            crate::host::Limits {
                                close_timeout: options.drain_budget,
                                ..Default::default()
                            },
                            Some("pilotdeck-agent-loop".into()),
                            move |identity, recorder, signal, authority| match authority {
                                Some(authority) => ports.turn_with_authority(
                                    identity,
                                    recorder,
                                    signal,
                                    Some(authority),
                                ),
                                None => turn_ports(&ports, identity, recorder, signal),
                            },
                        )
                        .with_pinned_generation(pinned_generation)
                        .with_native_terminal_errors()
                        .with_agent_config(config)
                        // #1864 (b): TS re-reads the permission-mode override
                        // on every run (InProcessGateway.ts:1443), so an
                        // already-live session picks the change up without a
                        // close. The source reads the same store the selector
                        // method writes.
                        .with_permission_mode_source(permission_source))
                    }),
                })
            })
        }),
        create_storage: Arc::new(move |context| {
            let fork_scope = storage_forks.scope(&context);
            let project = storage_project.clone();
            let options = storage_options.clone();
            Box::pin(async move {
                if let Some(scope) = fork_scope {
                    return Ok(SessionStorage::Unlocked(
                        crate::runtime::session::factory::FactoryStorage::new(
                            scope.store.clone(),
                            scope.key.clone(),
                        ),
                    ));
                }
                let lease = Arc::new(project.acquire(None)?);
                let resource = lease
                    .runtime()
                    .resources()
                    .get::<ProjectResources>()
                    .ok_or_else(|| RuntimeError::new("Native persistence project is missing."))?
                    .context()
                    .resource(Resource::Persistence)
                    .ok_or_else(|| RuntimeError::new("Native persistence is missing."))?;
                let (chat_dir, permit) = super::methods::session_dir::resolve_chat_dir(
                    options.home.clone(), options.project.clone()
                ).await.map_err(|error| RuntimeError::new(String::from_utf16_lossy(error.message.units())))?;
                let path = chat_dir.join(format!(
                    "{}.jsonl", crate::storage::sanitize_session_id(&context.session_key)
                ));
                let open_path = path.clone();
                let check_home = options.home.clone();
                let check_project = options.project.clone();
                let permit = Arc::new(permit);
                Ok(SessionStorage::Recording(RecordingPlan {
                    transcript_path: path,
                    open: Arc::new(move || {
                        let _permit = &permit;
                        let _lease = &lease;
                        // A marker may change after lookup while waiting for the
                        // writer lease. Refuse opening a different identity.
                        let current = super::methods::session_dir::bounded_chat_dir(&check_home, &check_project)
                            .map_err(|error| crate::storage::recorder::RecorderError::Type(
                                String::from_utf16_lossy(error.message.units())
                            ))?;
                        if current != chat_dir {
                            return Err(crate::storage::recorder::RecorderError::Type(
                                "Native session project marker changed while opening.".into()
                            ));
                        }
                        let persistence = resource
                            .as_any()
                            .downcast_ref::<PersistenceResources>()
                            .ok_or_else(|| {
                                crate::storage::recorder::RecorderError::Type(
                                    "Native persistence has the wrong type.".into(),
                                )
                            })?;
                        persistence
                            .create_session_bounded(context.session_key.clone(), Some(open_path.clone()))
                    }),
                }))
            })
        }),
        admission: None,
        tool_results_dir: Some(Arc::new(move |session_id| {
            super::tools::session_tool_results_dir(&budget_project, session_id)
        })),
        diagnostic: Arc::new(|d| eprintln!("native session: {d:?}")),
    }).with_publication(Arc::new(move|context,handle|{
        publication_forks.resume_published(context,handle)
    })));
    fork_registry.install_factory(&factory);
    let built = (|| -> Result<_, ProxyError> {
        // TS parity: an unconfigured runRegistry is simply absent, so the file
        // registry is never opened and no run persistence happens at all.
        let runs = match registry_switch {
            Some(RunRegistrySwitch::Off) => None,
            Some(RunRegistrySwitch::On) | None => Some(Arc::new(FileRegistry::new(
                FileRunRegistry::open(options.home.join("runs.json"))?,
            )?)),
        };
        let artifacts = Arc::new(GatewayToolResultArtifactStore::new(
            options.home.join("tool-results"),
            4096,
        )?);
        let projector = Arc::new(GatewayAgentEventProjector::new(
            GatewayAgentEventProjectorOptions {
                tool_result_artifacts: Some(artifacts),
                ..Default::default()
            },
        )?);
        // U1 upload store: resolves the configured project only.
        let upload_project = options.project.clone();
        let upload_store = Arc::new(crate::upload::UploadStore::new(
            crate::upload::UploadStoreOptions::new(
                move |key: &pilotdeck_protocol::JsString| {
                    // Shared helper `Unknown projectKey: <raw UTF-16>`
                    // (UploadStore.ts:63, #865 frozen behavior): the other
                    // 15+ call sites already report the exact UTF-16 units.
                    let path = match key.to_utf8() {
                        Ok(utf8) => std::path::PathBuf::from(utf8),
                        Err(_) => return Err(crate::upload::unknown_project_key(key)),
                    };
                    // Accept both the canonical path and the logical path.
                    let canonical = std::fs::canonicalize(&upload_project)
                        .unwrap_or_else(|_| upload_project.clone());
                    let requested = std::fs::canonicalize(&path).unwrap_or(path.clone());
                    if requested != canonical && path != upload_project {
                        return Err(crate::upload::unknown_project_key(key));
                    }
                    Ok(upload_project.clone())
                },
                {
                    let p = options.project.clone();
                    move || Ok(vec![p.clone()])
                },
            ),
        ));
        let upload_gateway = Arc::new(crate::upload::gateway::UploadGateway::new(
            upload_store.clone(),
        ));
        Ok((runs, projector, upload_store, upload_gateway))
    })();
    let (runs, projector, upload_store, upload_gateway) = match built {
        Ok(value) => value,
        Err(error) => {
            let _ = tokio::time::timeout(options.drain_budget, project.dispose()).await;
            return Err(error);
        }
    };
    // TS ui/server/routes/uploads.js:73-76: the store sweeps its own TTLs on a
    // detached schedule (#1439); the JoinHandle is dropped, not awaited. An
    // idle tick never blocks shutdown, but a sweep already running inside
    // `spawn_blocking` is not cancelled by the drop — see `spawn_cleanup`.
    if mode == AssemblyMode::Server {
        crate::upload::spawn_cleanup(&upload_store, upload_cleanup_interval);
    }
    let cleanup_factory = factory.clone();
    let cleanup_project = project.clone();
    // Track completed resources, not merely entry into cleanup: cancellation
    // before a later phase must not suppress that resource's failure fallback.
    let factory_finished: Arc<std::sync::atomic::AtomicBool> = Arc::default();
    let project_finished: Arc<std::sync::atomic::AtomicBool> = Arc::default();
    let fallback_factory_finished = factory_finished.clone();
    let fallback_project_finished = project_finished.clone();
    let fallback_observer = cleanup_observer.clone();
    let build_factory = factory.clone();
    let build_project = project.clone();
    // #1328 A8.3: the channel assembly (ChannelAdapterBundle weixin subset
    // on the #1327 ChannelServer). The same config snapshot drives the
    // `adapters.weixin.enabled` startup gate and the ChannelStartDeps config;
    // #1326 takes the `cron` section off the same load.
    let (channel_assembly, cron_section) = {
        use crate::runtime::project::ConfigSource;
        let config = crate::runtime::project::NativeConfigSource::new(
            options.project.clone(),
            env.clone(),
            Some(options.home.clone()),
            None,
        )
        .load(&options.project);
        let config = match config {
            Ok(config) => config,
            Err(error) => {
                let _ = tokio::time::timeout(options.drain_budget, project.dispose()).await;
                return Err(error.into());
            }
        };
        let config = config
            .get("config")
            .cloned()
            .unwrap_or_else(|| JsValue::Object(JsObject::new()));
        // #1302 B / #1386 / #1694 §7: a config that enables a channel Rust
        // does not implement refuses startup here, before `NativeChannels` is
        // constructed and therefore before any adapter construction or
        // `console.error` line. The error propagates out of `start` (and so
        // out of `native_process::run`), which is the observable output: the
        // process exits nonzero naming the key instead of starting with a
        // channel that will never respond. The second check covers the
        // bundle keys outside `UNMIGRATED_CHANNEL_KEYS` (`weixin` is built,
        // `feishu` is not yet — #1329).
        let rejected = if mode == AssemblyMode::Server {
            crate::channel::loader::reject_unmigrated_channels(config.get("adapters")).and_then(
                |()| {
                    crate::channel::loader::reject_unimplemented_bundle_channels(
                        config.get("adapters"),
                        super::channels::IMPLEMENTED_BUNDLE_CHANNEL_KEYS,
                    )
                },
            )
        } else {
            Ok(())
        };
        if let Err(error) = rejected {
            // Same teardown order as the failure branch below (#1328): the
            // session factory first, then the project registry.
            let _ = tokio::time::timeout(options.drain_budget, async {
                let _ = factory.dispose().await;
                project.dispose().await
            })
            .await;
            return Err(error.into());
        }
        // #1326 A8 U8: the `cron:` section travels to the builder below, which
        // is the only place that constructs `CronManager`.
        let cron_section = if mode == AssemblyMode::Server {
            config.get("cron").cloned()
        } else {
            None
        };
        let channel_assembly = Arc::new(super::channels::NativeChannels::new(
            super::channels::NativeChannelsOptions {
                pilot_home: options.home.clone(),
                config: Arc::new(config),
                weixin: crate::channel::weixin::WeixinChannelOptions::default(),
                weixin_maker: None,
                feishu: crate::channel::feishu::FeishuOptions::default(),
                log: None,
                report_status: None,
                close_host: None,
            },
        ));
        // #1329: hand the listener the slot before any adapter starts, so the
        // route is mounted with the process rather than with the channel.
        if let Some(slot) = webhook_slot {
            let _ = slot.set(channel_assembly.feishu_webhook_slot());
        }
        // #1353 A8.5-pr2: the channel assembly is the machine's E4 face
        // (close, the admission stop) and E3 face (flush, the durable state) —
        // handed out here, where it is constructed, so the embedding can
        // drive the switch without knowing the assembly's build order.
        if let Some(slots) = &switch_slots {
            let _ = slots.channels.set(channel_assembly.clone());
        }
        (channel_assembly, cron_section)
    };
    let build_channels = channel_assembly.clone();
    let cleanup_channels = channel_assembly.clone();
    // #1326: the builder constructs the one `CronManager` on the gateway
    // thread, but this cleanup closure is built before the builder runs —
    // the shared slot carries the started manager's stop handle from the
    // builder to the cleanup, which stops it before the channels (automation
    // precedes closeServer, PilotDeckServerShutdownBundle.ts:33-37).
    // #1353 A8.5-pr2: the slot now carries the shared idempotent stop handle
    // rather than the bare manager — the switch machine's S1 stops the
    // manager first, and the host teardown observes that stop instead of
    // repeating it.
    let build_cron_slot: Arc<OnceLock<Arc<crate::switch::CronStopHandle>>> = match &switch_slots {
        Some(slots) => slots.cron.clone(),
        None => Arc::default(),
    };
    let cleanup_cron_slot = build_cron_slot.clone();
    let lookup_forks = fork_registry.clone();
    let budget = options.drain_budget;
    let host = TurnGateway::start_with_cleanup_budget(
        TurnReplayOptions::new(),
        // #1746: the failure path waits out this embedding's own drain budget
        // (the cleanup below bounds itself by the same value), so a slow but
        // legal `--drain-ms` teardown is not cut by the host's default bound.
        budget,
        move |coordinator| {
            session_events.install(&coordinator);
            let owner = SessionOwner::new(SessionOwnerOptions::new(build_factory));
            lookup_forks.install_session_lookup(owner.weak_handle_lookup());
            lookup_forks.install_async_turn_lookup(owner.weak_async_turn_lookup());
            let replacement = Arc::new(ReplacementCoordinator::new(
                Default::default(),
                Arc::new(owner.clone()),
                budget.as_millis() as u64,
                None,
            ));
            let admission = SubmitAdmissionOptions {
                interactions: interactions.clone(),
                sdk_config: Some(sdk_port.clone()),
                ..SubmitAdmissionOptions::new(replacement.clone())
            };
            let services = Arc::new(super::services::Services {
                options: options.clone(),
                project: build_project.clone(),
                env: env.clone(),
                recorders: recorders.clone(),
                owner: owner.clone(),
                snapshot_recorder: snapshot_recorder.clone(),
                upload_store: upload_store.clone(),
            });
            let mcp_gateway = mcp::build_mcp_gateway(
                &owner,
                &build_project,
                &permission_rule_sets,
                &build_mcp_config,
                sdk_projects,
            );
            let async_hook_registry =
                steer::build_async_hook_registry(&build_project, &owner, &coordinator);
            let _ = sdk_async_hooks.set(async_hook_registry.clone());
            // U7 global settings (#1320): `createPilotConfigStoreSync` +
            // `runtimeRefresh` + the settings gateway
            // (createLocalGateway.ts:795-835, 1383-1410), composed in
            // `services::native_settings_for_process`; a store load failure
            // fails assembly, like the TS boot.
            let style_reload = Arc::new(style_reload::NativeStyleReload {
                options: options.clone(),
                project: build_project.clone(),
                owner: owner.clone(),
                interactions: interactions.clone(),
                sdk: sdk_port.clone(),
                overrides: session_overrides.clone(),
            });
            let settings = super::services::native_settings_for_process_with_styles(
                owner.clone(),
                build_project.clone(),
                &options.home,
                &options.project,
                &env,
                Some(session_overrides.clone()),
                (
                    Some(style_reload),
                    Some(Arc::new(sdk_dispatch::NativeSdkConfigDispatch(
                        sdk_handlers.clone(),
                    ))),
                ),
            )?;
            // #1326 A8 U8: the one `CronManager::new` of the native process.
            // `None` (no `cron:` section) keeps the unconfigured shape — the
            // six names stay out of capabilities and every call answers the
            // `requireCron` gate. A build failure fails the whole assembly
            // through the same `project.dispose()` fallback below.
            let cron = cron::build(cron_section.as_ref(), &options.home)?;
            // #1326: the cron port's own volatile fence. It must NOT be the
            // owner's `SessionAdmission`: `SessionAdmissionCronGateway` holds
            // its fence for the whole of `run_turn`, and that call ends in
            // `TurnSubmitter::submit`, which admits the very same sessionKey
            // through `SessionOwner::begin_turn`. Sharing one instance puts
            // the outer fence in the inner admission's way — the submit is
            // rejected `session_busy` and the turn never runs (review r1,
            // both channels). A dedicated instance still rejects a second
            // concurrent cron trigger for one sessionKey (cell 11), while the
            // real per-session admission stays where it belongs, inside the
            // submit path.
            let cron_admission = Arc::new(crate::runtime::session::SessionAdmission::default());
            let cron_owner = owner.clone();
            let execution_options = options.clone();
            let requests = Arc::new(super::requests::Requests {
                project: build_project,
                snapshot_enabled,
                memory: memory.clone(),
                options,
                env,
                coordinator: coordinator.clone(),
                runs: runs.clone(),
                owner: owner.clone(),
                replacement: replacement.clone(),
                recorders,
                interactions: interactions.clone(),
                mcp: mcp_gateway,
                async_hook_registry,
                user_dialog_bus: user_dialog_bus.clone(),
                upload_gateway: upload_gateway.clone(),
                // The CLI `prepareWeixinLogin` callback (pilotdeck.ts:240-247):
                // hot-start the weixin channel on request, after the same
                // durable-state restore as a config-enabled start.
                prepare_weixin_login: (mode == AssemblyMode::Server)
                    .then(|| build_channels.prepare_login_callback()),
                cron: cron.clone(),
                settings: settings.gateway,
                session_overrides,
                clear_sdk_state: Some(Arc::new(move |session| {
                    let sdk = sdk_port.clone();
                    Box::pin(async move {
                        sdk.registry
                            .lock()
                            .await
                            .clear_session_state(&session)
                            .map_err(|error| {
                                crate::runtime::connection::RequestError::new(error.message)
                            })
                    })
                })),
            });
            let submitter = Arc::new(TurnSubmitter {
                owner,
                coordinator,
                registry: runs.map(|runs| runs as Arc<dyn TurnRunRegistry>),
                workspace_admissions: WorkspaceAdmissions::default(),
                host: Arc::new(GatewaySubmitHost::new(
                    services,
                    Arc::new(NativeSubmitReplacement((*replacement).clone())),
                    Arc::new(GatewaySubmitAdmission::new(admission)),
                )),
                projector,
            });
            // #1326: bind the live gateway port before `start()` — the
            // manager's own bind gate enforces the order. `cron_admission` is
            // the port's dedicated fence (see above, not the owner's), so a
            // second concurrent cron trigger for one sessionKey is rejected
            // while the turn's real admission still happens in the submit
            // path; `start()` failing here fails the assembly through the
            // `project.dispose()` fallback below.
            if let Some(cron) = cron.as_ref() {
                let body = Arc::new(cron::NativeCronGatewayBody::new(
                    cron_owner.clone(),
                    submitter.clone(),
                    tokio::runtime::Handle::current(),
                ));
                let cron_shutdown = body.shutdown_signal();
                // #1353 A7: the recording wrapper the switch machine's S1
                // face reads. `CronManager::stop` aborts in-flight runs
                // through the bound gateway and swallows every rejection
                // (the `let _ =` in manager.rs stop), so the rejection has
                // to be recorded where the face can report it. Delegation is
                // behavior-preserving.
                let recording = Arc::new(crate::switch::RecordingCronGateway::wrap(Arc::new(
                    crate::runtime::cron::SessionAdmissionCronGateway::new(
                        cron_admission.clone(),
                        body,
                    ),
                )));
                cron.bind_agent_gateway(recording.clone())
                    .map_err(cron::cron_io_error)?;
                cron.set_execution_project_filter(Arc::new(move |project| {
                    execution_options.serves_project(project)
                }))
                .map_err(cron::cron_io_error)?;
                cron::bind_callbacks(
                    cron,
                    build_channels.server().clone(),
                    tokio::runtime::Handle::current(),
                    cron_shutdown.clone(),
                )
                .map_err(cron::cron_io_error)?;
                cron.start().map_err(cron::cron_io_error)?;
                build_cron_slot
                    .set(Arc::new(crate::switch::CronStopHandle::new_native(
                        cron.clone(),
                        recording.failures(),
                        std::time::Duration::from_secs(15),
                        Arc::new(move || cron_shutdown.cancel()),
                    )))
                    .map_err(|_| std::io::Error::other("cron manager slot already occupied"))?;
            }
            // #1328: publish the weixin gateway port (the TS `baseDeps.gateway`
            // every channel start receives) before any channel can construct.
            // The builder runs exactly once per host, so a second publish is
            // a wiring bug, not a value to ignore.
            if !build_channels.set_gateway_port(Arc::new(
                crate::channel::weixin::NativeWeixinGatewayPort::new(submitter.clone()),
            )) {
                return Err(std::io::Error::other(
                    "weixin gateway port already published",
                ));
            }
            // #1329: the same submitter answers `/feishu/webhook` turns; the
            // adapter holds the handle from construction, so this only fills
            // the slot behind it.
            if !build_channels.set_turn_submitter(submitter.clone()) {
                return Err(std::io::Error::other(
                    "feishu turn gateway already published",
                ));
            }
            Ok(GatewayServices {
                interactions,
                requests,
                submitter,
            })
        },
        async move {
            // #1746: the host awaits this future on its own failure path before
            // reporting the error, so the deadline lives here; the shutdown
            // drain still bounds it by the embedding's shutdown budget.
            let teardown = async move {
                // #1326 A8 U8: stop the cron manager first — automation precedes
                // closeServer (PilotDeckServerShutdownBundle.ts:33-37). `stop()`
                // aborts active cron runs through the bound gateway (the
                // synchronous abort prefix) and releases task sessions; a stop
                // failure is observed but never blocks the remaining teardown.
                // #1353: through the shared handle — after a machine-driven
                // stop this observes the earlier stop instead of repeating it.
                let cron_stop = match cleanup_cron_slot.get() {
                    Some(handle) => {
                        cleanup_observer(
                            "cron",
                            handle
                                .stop_async()
                                .await
                                .map_err(|error| String::from_utf16_lossy(error.message().units())),
                        )
                        .await
                    }
                    None => Ok(()),
                };
                // #1328 A8.3: stop the channel assembly and flush its durable
                // mapper state — the closeServer + flushChannelState pair
                // (PilotDeckServerShutdownBundle.ts:33-37: channels stop before
                // the gateway disposes). The WS server has already drained when
                // this cleanup runs; the close-host hook is the pr2 WS-ordering
                // follow-up.
                let channel_stop = cleanup_observer(
                    "channels",
                    cleanup_channels
                        .close()
                        .await
                        .map_err(|e| e.message().to_string()),
                )
                .await;
                let channel_flush = cleanup_channels.flush().await;
                let sessions = cleanup_factory.dispose().await.map_err(|e| e.to_string());
                factory_finished.store(true, std::sync::atomic::Ordering::Release);
                let sessions = cleanup_observer("session", sessions).await;
                let project = cleanup_project.dispose().await.map_err(|e| e.to_string());
                project_finished.store(true, std::sync::atomic::Ordering::Release);
                let project = cleanup_observer("project", project).await;
                if let Err(error) = &project {
                    cleanup_diagnostic(error);
                }
                let mut outcome = sessions.and(project);
                for failure in [cron_stop.err(), channel_stop.err(), channel_flush.err()]
                    .into_iter()
                    .flatten()
                {
                    outcome = outcome.and(Err(failure));
                }
                outcome
            };
            match tokio::time::timeout(budget, teardown).await {
                Ok(outcome) => outcome,
                Err(_) => Err("gateway host cleanup deadline exceeded".to_string()),
            }
        },
    );
    match host {
        Ok(host) => {
            // #1328: start config-enabled channels (pilotdeckServer.ts:288-296)
            // once the builder has published the gateway port.
            let mut startup = Vec::new();
            if mode == AssemblyMode::Server {
                if let Some(feishu) = channel_assembly.create_startup_feishu().await {
                    startup.push(feishu);
                }
                if let Some(weixin) = channel_assembly.create_startup_weixin().await {
                    startup.push(weixin);
                }
            }
            if !startup.is_empty() {
                channel_assembly.server().start_channels(startup);
            }
            Ok(host)
        }
        Err(error) => {
            // The serial host has stopped polling its cleanup before reporting
            // failure. Each unfinished resource gets its own bounded attempt;
            // a stalled observer cannot repeat an already disposed resource.
            if !fallback_factory_finished.load(std::sync::atomic::Ordering::Acquire) {
                let _ = tokio::time::timeout(budget, async {
                    let result = factory.dispose().await.map_err(|e| e.to_string());
                    fallback_factory_finished.store(true, std::sync::atomic::Ordering::Release);
                    fallback_observer("session", result).await
                })
                .await;
            }
            if !fallback_project_finished.load(std::sync::atomic::Ordering::Acquire) {
                let _ = tokio::time::timeout(budget, async {
                    let result = project.dispose().await.map_err(|e| e.to_string());
                    fallback_project_finished.store(true, std::sync::atomic::Ordering::Release);
                    fallback_observer("project", result).await
                })
                .await;
            }
            Err(error.into())
        }
    }
}
/// #1864 (a): the pure compose step for the three session selections —
/// extracted from the prepare closure so the read-back tests exercise the
/// same logic the next session runs. Mirrors
/// SessionAgentConfigBundle.compose: `managedPermissions.defaultMode` (a
/// non-empty string) wins over the stored permission mode, else "default"
/// (shared: `runtime::permission::effective_permission_mode`);
/// a stored thinking value is written as-is (JS null included — presence is
/// the override, TS deletes only on undefined); the output style's content
/// merges into `appendSystemPrompt` as
/// `[existing, content].filter(non-blank).join("\n\n")`, an empty join
/// writes undefined, and a style that resolved to no content leaves the
/// field untouched.
pub(super) fn apply_session_overrides(
    config: &mut JsObject,
    stored_permission_mode: Option<&JsString>,
    stored_thinking: Option<&JsValue>,
    style_content: Option<&str>,
) {
    let mode = effective_permission_mode(config, stored_permission_mode.cloned());
    config.insert("permissionMode".into(), JsValue::String(mode));
    if let Some(thinking) = stored_thinking {
        config.insert("thinking".into(), thinking.clone());
    }
    if let Some(content) = style_content {
        let existing = config
            .get("appendSystemPrompt")
            .and_then(JsValue::as_string)
            .map(|value| String::from_utf16_lossy(value.units()));
        let joined = [existing.as_deref(), Some(content)]
            .into_iter()
            .flatten()
            .filter(|part| !crate::metadata::js_trim(part).is_empty())
            .collect::<Vec<_>>()
            .join("\n\n");
        config.insert(
            "appendSystemPrompt".into(),
            if joined.is_empty() {
                JsValue::Undefined
            } else {
                JsValue::String(joined.into())
            },
        );
    }
}

/// #1864: the stored output style's content through the same view
/// `output_styles_list` lists (ProjectResources → PluginRuntime →
/// PluginResources.runtime). Like ProjectRuntimeRegistry's
/// getSdkOutputStyleContent the view is read without a refresh and a
/// missing style is not an error: `None` leaves the field alone.
async fn output_style_content(staged: &StageContext, name: &JsString) -> Option<String> {
    let plugin_runtime = staged
        .resource(Resource::PluginRuntime)?
        .as_any()
        .downcast_ref::<PluginResources>()?
        .runtime
        .clone();
    plugin_runtime
        .create_view(vec![])
        .await
        .list_output_styles()
        .into_iter()
        .find(|style| JsString::from(style.name.as_str()) == *name)
        .map(|style| style.content)
}
pub(super) fn register_project(options: &Options) -> Result<(), ProxyError> {
    // resolve_process_path reads current_dir only for a relative marker.
    register_project_at(options, std::path::Path::new(""))
}

pub(super) fn register_project_at(
    options: &Options,
    cwd: &std::path::Path,
) -> Result<(), ProxyError> {
    let chats =
        super::methods::session_dir::bounded_chat_dir_at(&options.home, &options.project, cwd)
            .map_err(|error| String::from_utf16_lossy(error.message.units()))?;
    let directory = chats
        .parent()
        .ok_or("Native project storage has no parent.")?;
    std::fs::create_dir_all(directory)?;
    let marker = directory.join(".cwd");
    if marker.exists() {
        let files = super::methods::session_dir::BoundedSessionMarkers::new();
        let bytes = crate::metadata::Files::read(&files, &marker)?;
        let existing = String::from_utf8(bytes)
            .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
        let existing = crate::metadata::js_trim(&existing);
        // TS registration lookup rejects blank markers before resolving paths.
        // Resolving "" when cwd is the project would otherwise manufacture ownership.
        if existing.is_empty() {
            return Err("Native project storage marker is empty.".into());
        }
        // TS stat(marker) / resolve(marker) interpret relative markers against
        // the process cwd, not the project requested by the caller.
        if crate::metadata::resolve_process_path(cwd, std::path::Path::new(existing))?
            != options.project
        {
            return Err("Native project storage marker belongs to a different project.".into());
        }
        return Ok(());
    }
    // Publish all bytes atomically, without replacing another writer's marker.
    let temporary = directory.join(format!(".cwd-{}", uuid::Uuid::new_v4()));
    let publish = (|| -> std::io::Result<()> {
        use std::io::Write;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)?;
        file.write_all(options.project.to_string_lossy().as_bytes())?;
        file.sync_all()?;
        std::fs::hard_link(&temporary, &marker)
    })();
    let _ = std::fs::remove_file(&temporary);
    publish?;
    Ok(())
}
/// This entry's currently supported profile is a text main agent with native
/// read tools. Refuse configured integrations before admitting a turn instead
/// of silently dropping their tools/hooks or launching an implicit helper.
pub(super) async fn validate_profile(options: &Options, env: &Env) -> Result<(), ProxyError> {
    validate_profile_inner(options, env, false).await
}

async fn validate_profile_inner(
    options: &Options,
    env: &Env,
    _management: bool,
) -> Result<(), ProxyError> {
    // The native plugin generation independently rechecks this restricted
    // profile and never loads non-style families, closing the check/load race.
    let home = options.home.clone();
    let project = options.project.clone();
    let permit = crate::runtime::plugins::style_slots()
        .acquire_owned()
        .await
        .map_err(|_| "Style validation is unavailable.")?;
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        let paths = crate::runtime::plugins::PilotExtensionPaths::resolve(&project, &home);
        let mut budget = crate::runtime::plugins::loader::StyleBudget::new();
        crate::runtime::plugins::loader::validate_native_style_paths(
            &[
                (
                    paths.global_plugins_dir,
                    crate::runtime::plugins::PluginSource::Global,
                ),
                (
                    paths.project_plugins_dir,
                    crate::runtime::plugins::PluginSource::Project,
                ),
            ],
            &mut budget,
        )
        .map_err(|error| error.to_string())?;
        Ok::<(), String>(())
    })
    .await
    .map_err(|error| error.to_string())??;
    use crate::runtime::project::ConfigSource;
    let snapshot = crate::runtime::project::NativeConfigSource::new(
        options.project.clone(),
        env.clone(),
        Some(options.home.clone()),
        None,
    )
    .load(&options.project)?;
    // Unknown top-level hooks produce a TS diagnostic; only plugin and SDK
    // contribution authorities may supply executable hooks.
    if snapshot
        .get("config")
        .and_then(|v| v.get("modules"))
        .and_then(JsValue::as_object)
        .is_some_and(|modules| {
            modules
                .own_entries()
                .iter()
                .any(|(name, value)| !supports_native_module_binding(name, value))
        })
    {
        return Err(
            "Native isolated profile does not yet support configured module bindings.".into(),
        );
    }
    Ok(())
}
// These are the normalized native bindings from parseModulesConfig. In the
// TS runtime they retain the same default owner as an absent binding; only an
// external protocol binding replaces a model/tool/context/agent-loop port.
fn supports_native_module_binding(name: &JsString, value: &JsValue) -> bool {
    let Some(fields) = value.as_object() else {
        return false;
    };
    if (name.equals_str("skills") || name.equals_str("knowledge"))
        && fields.own_entries().len() == 1
        && fields.get("enabled") == Some(&JsValue::Bool(false))
    {
        return true;
    }
    [
        "agentLoop",
        "skills",
        "modelProvider",
        "tools",
        "context",
        "knowledge",
    ]
    .iter()
    .any(|slot| name.equals_str(slot))
        && fields.own_entries().len() == 2
        && fields.get("enabled") == Some(&JsValue::Bool(true))
        && fields
            .get("provider")
            .and_then(JsValue::as_string)
            .is_some_and(|provider| provider.equals_str("pilotdeck"))
}

pub fn env_object(env: &Env) -> JsObject {
    let mut result = JsObject::new();
    for (key, value) in env {
        result.insert(key.as_str().into(), JsValue::String(value.as_str().into()));
    }
    result
}
pub fn value<const N: usize>(entries: [(&'static str, JsValue); N]) -> JsObject {
    object(entries)
        .as_object()
        .expect("object constructor")
        .clone()
}

pub(super) fn turn_ports(
    ports: &super::tools::SessionTools,
    identity: &pilotdeck_protocol::admission::ExecutionIdentity,
    recorder: Arc<Recorder>,
    signal: crate::runtime::hooks::Cancellation,
) -> crate::runtime::turn::host_factory::SessionPorts {
    ports.turn(identity, recorder, signal)
}

// LocalGatewayBootConfig resolves explicit env first, then install-adjacent
// skills, and finally <project>/skills. All consumers use the same result.
pub(super) fn explicit_builtin_skills_root(options: &Options, env: &Env) -> Option<PathBuf> {
    let exe_dir = std::env::current_exe()
        .ok()
        .map(|path| std::fs::canonicalize(&path).unwrap_or(path))
        .and_then(|path| path.parent().map(Path::to_path_buf));
    builtin_skills_root_at(&options.project, env, exe_dir.as_deref())
}

pub(super) fn builtin_skills_root_at(
    project: &Path,
    env: &Env,
    exe_dir: Option<&Path>,
) -> Option<PathBuf> {
    if let Some(configured) = env
        .get("PILOTDECK_BUNDLED_SKILLS_DIR")
        .filter(|value| !value.is_empty())
    {
        return crate::metadata::resolve_process_path(project, Path::new(configured)).ok();
    }
    if let Some(exe_dir) = exe_dir {
        for relative in ["../skills", "../../skills"] {
            if let Ok(candidate) =
                crate::metadata::resolve_process_path(project, &exe_dir.join(relative))
                && candidate.exists()
            {
                return Some(candidate);
            }
        }
    }
    crate::metadata::resolve_process_path(project, Path::new("skills")).ok()
}

pub(super) fn project_registry(options: &Options, env: &Env) -> Arc<ProjectRuntimeRegistry> {
    Arc::new(ProjectRuntimeRegistry::new(
        options.project.clone(),
        Arc::new(ProjectResourcesFactory::new(ProjectResourcesOptions {
            pilot_home: options.home.clone(),
            cwd: options.project.clone(),
            env: env.clone(),
            homedir: Some(options.home.clone()),
            builtin_skills_root: explicit_builtin_skills_root(options, env),
            now: Arc::new(SystemTime::now),
            telemetry: None,
            config_version: None,
            runtime_profile_overrides: RuntimeProfileOverrides::default(),
            families: Families {
                lsp: Some(Arc::new(LspFamily)),
                session_title: Some(Arc::new(TitleFamily)),
                ..Families::unwired()
            }
            .with_native_models()
            .with_context_memory()
            .with_tools_mcp()
            .with_native_background_tasks()
            .with_native_style_plugins(crate::runtime::plugins::PluginRuntimeOptions {
                project_root: options.project.clone(),
                pilot_home: options.home.clone(),
                builtin_skills_root: explicit_builtin_skills_root(options, env),
                builtin_plugins: vec![],
                builtin_plugins_enabled: HashMap::new(),
            })
            .with_interaction(),
        })),
    ))
}
