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
            ProjectRuntimeRegistry, Resource, RuntimeError, RuntimeProfileOverrides,
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

mod always_on;
pub(super) mod automation;
mod automation_lifetime;
mod cron;
pub(super) mod dialog_recovery;
mod mcp;
mod memory;
mod prompt_suggestion;
mod sdk_adapter;
mod sdk_dispatch;
pub(super) mod sdk_hooks;
mod sdk_model;
mod sdk_plugins;
mod sdk_settings;
pub(crate) mod snapshot;
mod steer;
mod style_reload;
mod task_budget;
pub(super) mod user_dialog;

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

/// Compose the same native Server core for an embedding that owns a browser
/// manager. The caller owns its listener; an absent provider remains unavailable.
pub async fn start_with_manager_browser(
    options: Options,
    env: Env,
    manager_browser: Option<Arc<dyn crate::runtime::manager::ManagerBrowserPort>>,
) -> Result<TurnGateway, ProxyError> {
    start_assembly(
        options,
        env,
        crate::upload::CLEANUP_INTERVAL,
        Arc::new(|_, result| Box::pin(async move { result })),
        Arc::new(|error| eprintln!("native project cleanup: {error}")),
        None,
        None,
        AssemblySeams {
            manager_browser,
            ..Default::default()
        },
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
    #[cfg(test)]
    pub channel_endpoints: super::channels::ChannelEndpointOverrides,
    #[cfg(test)]
    pub channel_declarations: Option<ChannelDeclarations>,
    pub mode: AssemblyMode,
    pub permission_rule_sets: Option<Arc<GatewaySessionPermissionRuleSetRegistry>>,
    pub acquire_miss_report: Option<AcquireMissReport>,
    pub publish_mcp_config: bool,
    pub manager_browser: Option<Arc<dyn crate::runtime::manager::ManagerBrowserPort>>,
}

impl Default for AssemblySeams {
    fn default() -> Self {
        Self {
            #[cfg(test)]
            channel_endpoints: Default::default(),
            #[cfg(test)]
            channel_declarations: None,
            mode: AssemblyMode::Server,
            permission_rule_sets: None,
            acquire_miss_report: None,
            publish_mcp_config: true,
            manager_browser: None,
        }
    }
}

/// A reduced constructor set exercises the real startup refusal path even
/// when this build implements every channel. Production always uses its tables.
#[cfg(test)]
pub(super) struct ChannelDeclarations {
    pub unmigrated: &'static [&'static str],
    pub bundle: &'static [&'static str],
}

#[cfg(test)]
pub(super) async fn start_with_channel_declarations(
    options: Options,
    env: Env,
    declarations: ChannelDeclarations,
) -> Result<TurnGateway, ProxyError> {
    start_assembly(
        options,
        env,
        crate::upload::CLEANUP_INTERVAL,
        Arc::new(|_, result| Box::pin(async move { result })),
        Arc::new(|error| eprintln!("native project cleanup: {error}")),
        None,
        None,
        AssemblySeams {
            channel_declarations: Some(declarations),
            ..Default::default()
        },
    )
    .await
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
            approvals: switch_slots.approvals.get().cloned(),
            cron: switch_slots.cron.get().cloned(),
            automation: switch_slots.automation.get().cloned(),
            channels: switch_slots.channels.get().cloned().ok_or_else(|| {
                ProxyError::from("channel assembly slot was not constructed during assembly")
            })?,
        },
    ))
}

/// Real Server assembly with loopback platform origins, available only in tests.
#[cfg(test)]
pub(super) async fn start_with_channel_endpoints(
    options: Options,
    env: Env,
    channel_endpoints: super::channels::ChannelEndpointOverrides,
) -> Result<(TurnGateway, Arc<super::channels::NativeChannels>), ProxyError> {
    let slots = super::switch_wiring::AssemblySwitchSlots::default();
    let host = start_assembly(
        options,
        env,
        crate::upload::CLEANUP_INTERVAL,
        Arc::new(|_, result| Box::pin(async move { result })),
        Arc::new(|error| eprintln!("native project cleanup: {error}")),
        None,
        Some(slots.clone()),
        AssemblySeams {
            channel_endpoints,
            ..Default::default()
        },
    )
    .await?;
    let channels = slots
        .channels
        .get()
        .cloned()
        .ok_or_else(|| ProxyError::from("channel assembly slot unavailable"))?;
    Ok((host, channels))
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
            ..Default::default()
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
        #[cfg(test)]
        channel_endpoints,
        #[cfg(test)]
        channel_declarations,
        mode,
        permission_rule_sets,
        acquire_miss_report,
        publish_mcp_config,
        manager_browser,
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
    #[cfg(unix)]
    {
        let home = options.home.clone();
        match tokio::task::spawn_blocking(move || super::session_history::recover(&home)).await {
            Ok(Ok(report)) => {
                for failure in report.failures {
                    eprintln!(
                        "[gateway] transcript replacement recovery {}: {}",
                        failure.scope.display(),
                        failure.message
                    );
                }
            }
            Ok(Err(error)) => eprintln!("[gateway] transcript replacement recovery: {error}"),
            Err(error) => eprintln!("[gateway] transcript replacement recovery task: {error}"),
        }
    }
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
    let sop =
        super::sop_control::NativeSopControl::new(options.clone(), env.clone(), project.clone())
            .map_err(|error| {
                String::from_utf16_lossy(super::sop_control::request_error(error).message.units())
            })?;
    if let Some(slots) = &switch_slots {
        slots
            .approvals
            .set(sop.bridge())
            .map_err(|_| "SOP approval slot is already occupied")?;
    }
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
    let notifications = (mode == AssemblyMode::Server)
        .then(|| Arc::new(super::notifications::NativeNotifications::default()));
    let server_notifications = notifications.as_ref().map(|notifications| {
        notifications.clone() as Arc<dyn crate::runtime::refresh::NotificationServer>
    });
    let turn_broadcast = notifications
        .as_ref()
        .map(|notifications| notifications.turn_events());
    let worktree_broadcast = notifications
        .as_ref()
        .map(|notifications| notifications.worktree_events());
    let user_dialog_bus = user_dialog::build_user_dialog_bus(server_notifications.clone());
    // #1864: the per-gateway session selection store. One instance serves
    // both writers (the three selector methods on `Requests`) and the reader
    // (the prepare closure composing each session's agent config).
    let session_overrides: Arc<super::SessionOverrides> = Arc::default();
    let prepare_overrides = session_overrides.clone();
    let sdk_projects = SdkSessionProjects::shared();
    let budget_ledger = task_budget::NativeBudgetLedger::new(options.home.clone());
    let prepare_mcp_config: Arc<std::sync::OnceLock<Arc<SdkMcpConfig>>> = Arc::default();
    let normalize_mcp_config = prepare_mcp_config.clone();
    let style_projection = Arc::downgrade(&session_overrides);
    let sdk_options = crate::runtime::sdk_session::SdkSessionOptions::new(
        options.project.clone(),
        options.project.clone(),
        sdk_settings::source_environment(&env, &options.home),
        Arc::new(sdk_adapter::NativeSdkResolver {
            project: project.clone(),
        }),
    )
    .with_homedir(options.home.clone())
    .with_setting_source_reader(Arc::new(sdk_settings::NativeSettingSourceReader))
    .with_output_style_projection(Arc::new(move |key, style| {
        if let Some(owner) = style_projection.upgrade() {
            match style {
                Some(name) => owner.set_output_style(key, name),
                None => owner.clear_output_style(key),
            }
        }
    }))
    .with_plugin_loader(Arc::new(sdk_plugins::NativeSdkPluginLoader))
    .with_agent_normalizer(Arc::new(
        move |registry, session, config, runtime, skills| {
            sdk_adapter::resolve_native_agents(
                registry,
                session,
                config,
                runtime,
                skills,
                normalize_mcp_config.get(),
            )
        },
    ))
    .with_projects(sdk_projects.clone())
    .with_task_budget(budget_ledger.clone());
    let sdk_registry = crate::runtime::sdk_session::SdkSessionRegistry::new(sdk_options)
        .with_shared_handlers(sdk_handlers.clone());
    let sdk_async_hooks = Arc::new(std::sync::OnceLock::new());
    let sdk_port = Arc::new(sdk_adapter::NativeSdkPort {
        handlers: sdk_handlers.clone(),
        async_hooks: sdk_async_hooks.clone(),
        registry: tokio::sync::Mutex::new(sdk_registry),
    });
    let style_port: Arc<dyn super::session_overrides::SdkOutputStylePort> = sdk_port.clone();
    session_overrides.bind_sdk_styles(Arc::downgrade(&style_port));
    let prepare_sdk = sdk_port.clone();
    let ephemeral = Arc::new(super::sdk_ephemeral::EphemeralSessions::default());
    let prepare_ephemeral = ephemeral.clone();
    let cleanup_ephemeral = ephemeral.clone();
    let fallback_ephemeral = ephemeral.clone();
    let fork_registry = super::agent::NativeForkRegistry::new();
    let prepare_forks = fork_registry.clone();
    let session_events = Arc::new(super::session_events::NativeSessionEvents::default());
    let prepare_session_events = session_events.clone();
    let storage_forks = fork_registry.clone();
    let prepare_project = project.clone();
    let prepare_sop = sop.clone();
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
    let prepare_user_dialog_bus = user_dialog_bus.clone();
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
            let sop = prepare_sop.clone();
            let forks = prepare_forks.clone();
            let session_events = prepare_session_events.clone();
            let mut options = prepare_options.clone();
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
            let ephemeral=prepare_ephemeral.clone();
            let dialog_bus = prepare_user_dialog_bus.clone();
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
                    None => {
                        let root = context.project_key.as_ref().map(|key| key.to_utf8().map(PathBuf::from).map_err(|_| RuntimeError::new("Native session project identity is not representable."))).transpose()?.unwrap_or_else(|| options.project.clone());
                        project.acquire(Some(&root))?
                    }
                };
                options.project = lease.runtime().project_root().to_path_buf();
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
                let effective_sdk=sdk.resolved_prompt_config(&context.session_key,resources).await.map_err(|error|RuntimeError::new(error.message))?;
                let agent=sdk_settings::compose_agent(&agent,effective_sdk.as_ref(),&snapshot);
                let mut config = agent.as_object().cloned().unwrap_or_default();
                if let Some(sdk) = &effective_sdk { sdk_adapter::apply_prompt_config(&mut config,sdk); }

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
                let stored_style_name=session_overrides.effective_output_style(&context.session_key).await.map_err(|error|RuntimeError::new(error.message))?;
                apply_session_overrides(
                    &mut config,
                    stored_permission_mode.as_ref(),
                    stored_thinking.as_ref(),
                    None,
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
                let automation = session_overrides.automation(&context.session_key);
                if let Some(automation) = &automation {
                    config.insert("cwd".into(), automation.cwd.to_string_lossy().as_ref().into());
                    config.insert("permissionMode".into(), JsValue::String(automation.permission_mode.clone()));
                    config.insert("bypassAvailable".into(), JsValue::Bool(automation.bypass_available));
                    config.insert("canPrompt".into(), JsValue::Bool(automation.can_prompt));
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
                let session_plugins = sdk.registry.lock().await.session_plugins(&context.session_key).to_vec();
                let mut skill_lease = plugin_runtime
                    .acquire_session_contributions(session_plugins)
                    .await
                    .map_err(|error| RuntimeError::new(error.to_string()))?;
                let input_admission = Arc::new(super::input_admission::InputAdmission::new(
                    &skill_lease.contributions.commands,
                ));
                if fork_scope.is_none()
                    && let Some(name) = &stored_style_name
                    && let Some(style) = skill_lease.contributions.output_styles.iter()
                        .find(|style| JsString::from(style.name.as_str()) == *name)
                { apply_output_style_content(&mut config, &style.content); }
                let plugin_hooks = skill_lease.contributions.hooks.clone();
                let plugin_mcp_servers = skill_lease.contributions.mcp_servers.clone();
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
                let native_extensions = super::extensions::NativeExtensions::new(
                    native_skills.clone(),
                    &skill_lease.contributions.commands,
                    &skill_lease.contributions.mcp_instructions,
                );
                let context_storage = staged
                    .resource(Resource::InstructionStorage)
                    .ok_or_else(|| RuntimeError::new("Native context storage is missing."))?;
                let context_storage = context_storage.as_any()
                    .downcast_ref::<crate::runtime::project::context_storage::ContextStorageResources>()
                    .ok_or_else(|| RuntimeError::new("Native context storage has the wrong type."))?;
                let is_ephemeral = {let registry=sdk.registry.lock().await;registry.is_ephemeral_session(&context.session_key)};
                if fork_scope.is_none(){config.insert("nativeEphemeralRoot".into(),JsValue::Undefined);}
                let ephemeral_root = if fork_scope.is_none() && is_ephemeral {
                    let root=ephemeral.acquire(context.session_key.clone()).await?;
                    config.insert("nativeEphemeralRoot".into(),JsValue::String(root.path().to_string_lossy().into_owned().into()));Some(root)
                }else{None};
                let session_artifact_project=config.get("nativeEphemeralRoot").and_then(JsValue::as_string).and_then(|s|s.to_utf8().ok()).map(std::path::PathBuf::from).unwrap_or_else(||options.project.clone());
                let summary_model = crate::host::modules::model_port::ports(
                    router.router.clone(), router.model_config.clone(), None,
                    crate::host::modules::model_port::RouterModelPortOptions {
                        is_main_agent: false,
                        project_path: Some(options.project.to_string_lossy().into_owned().into()),
                        ..Default::default()
                    }, &context.session_key,
                ).execution;
                let compaction = crate::runtime::project::context_storage::native_compaction(
                    summary_model, context.session_key.clone(),
                    selected.get("provider").and_then(JsValue::as_string).cloned().ok_or_else(|| RuntimeError::new("Agent model provider is missing."))?,
                    selected.get("model").and_then(JsValue::as_string).cloned().ok_or_else(|| RuntimeError::new("Agent model name is missing."))?,
                );
                let native_context = Arc::new(
                    context_storage
                        .create_session_with_options(
                            super::tools::session_tool_results_dir(
                                &session_artifact_project,
                                &context.session_key,
                            ),
                            crate::runtime::project::context_storage::SessionContextOptions {
                                extension: Some(Arc::new(native_extensions)),
                                compaction: Some(compaction),
                                memory_disabled: config.get("nativeMemoryDisabled")
                                    == Some(&JsValue::Bool(true)),
                                ..Default::default()
                            },
                        )?
                        .context_runtime,
                );
                let mut retained = PreparedResources::default();
                let history_root = match &fork_scope {
                    Some(scope) => scope.history_root.clone(),
                    None => ephemeral_root.as_ref().map(|root| root.history_root()).transpose()?,
                };
                retained.enable_file_history(options.home.clone(), history_root);
                let fork_owner = super::agent::NativeForkOwner::new(lease);
                // Every native session, not just a fork, runs the exact
                // project generation that prepared its tools and permissions.
                let generation = fork_owner.generation().ok_or_else(|| RuntimeError::new("Native session generation is missing."))?;
                let pinned_generation = Some(Arc::downgrade(&generation));
                let storage_factory = if let Some(scope) = &fork_scope {
                    let store = scope.store.clone();
                    let key = scope.key.clone();
                    Arc::new(move |_| {
                        let store = store.clone();
                        let key = key.clone();
                        Box::pin(async move { Ok(SessionStorage::Unlocked(crate::runtime::session::factory::FactoryStorage::new(store, key))) }) as crate::runtime::refresh::BoxFuture<_>
                    }) as crate::runtime::session::factory::CreateStorage
                } else if let Some(root)=ephemeral_root {
                    super::project_storage::for_ephemeral(generation,root)
                } else {
                    ephemeral.durable_storage(super::project_storage::for_generation(options.home.clone(), generation))
                };
                retained.set_storage_factory(storage_factory)?;
                let admission_sop = sop.clone();
                let admission_snapshot = snapshot.clone();
                let admission_project: JsString = options.project.to_string_lossy().into_owned().into();
                retained.set_admission(Arc::new(move |_| {
                    let sop = admission_sop.clone();
                    let snapshot = admission_snapshot.clone();
                    let project = admission_project.clone();
                    Box::pin(async move { Ok(sop.creation_admission(&project, &snapshot).await) })
                }))?;
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
                    .unwrap_or_else(|| super::sdk_permission::rules(&config, &permission_lease.rules()));
                let native_permission_rules = match &automation {
                    Some(automation) => native_permission_rules.with_policy_deny(&automation.permission_deny),
                    None => native_permission_rules,
                };
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
                if let Some(mcp_config) = mcp_config_handle.get().filter(|_| !native_observer) {
                    super::sdk_tools::merge_mcp_deferred(&mut config, &super::enabled_session_mcp_servers(mcp_config, &context.session_key))
                        .map_err(|error| RuntimeError::new(error.message))?;
                }
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
                            plugin_servers: Some(&plugin_mcp_servers),
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
                let suggestions = sdk.prompt_config(&context.session_key).await
                    .as_ref().and_then(|sdk|sdk.get("promptSuggestions"))==Some(&JsValue::Bool(true));
                let suggestion_generator = suggestions.then(|| prompt_suggestion::suggestion_generator(
                    model.providers.clone(), object([
                        ("provider",config.get("provider").cloned().unwrap_or(JsValue::Undefined)),
                        ("model",config.get("model").cloned().unwrap_or(JsValue::Undefined)),
                    ])));
                let tail = TailConfig {
                    suggestion_generator,
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
                let native_dialog = if fork_scope.is_none() && config.get("userDialogKinds").is_some() {
                    let ephemeral = sdk.registry.lock().await.is_ephemeral_session(&context.session_key);
                    Some(user_dialog::prepare_dialogs(options.home.clone(), options.project.clone(),
                        context.session_key.clone(), ephemeral, dialog_bus).await?)
                } else { None };
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
                if let Some(dialog) = &native_dialog {
                    let port = dialog.port.clone();
                    retained.push(move || Box::pin(async move { port.close().await; Ok(()) }));
                }
                // The profile is the immutable snapshot that supplied this
                // session's model, tools, storage and original admission.
                let sop_profile=snapshot.get("config").and_then(|v|v.get("modules")).and_then(|v|v.get("sop")).filter(|v|!matches!(v,JsValue::Undefined|JsValue::Null)).cloned();
                let knowledge_binding = snapshot.get("config").and_then(|v|v.get("modules")).and_then(|v|v.get("knowledge")).filter(|binding| binding.get("transport") == Some(&JsValue::from("module-http-v2")));
                let (native_knowledge, sop_authority) = match knowledge_binding {
                    Some(binding) => {
                        let (tool, authority) = super::knowledge::build(binding, sop_profile.as_ref(), &env, hook_proxy.as_ref())
                            .map_err(|error| RuntimeError::new(String::from_utf16_lossy(error.message.units())))?;
                        (Some(tool), authority)
                    }
                    None => (None, None),
                };
                let sop_controller=if config.get("isSubagent")!=Some(&JsValue::Bool(true)) {
                    match sop_profile {
                        Some(profile)=>{
                            let controller = crate::runtime::sop::controller::StaffDeckController::load(profile,Arc::new(crate::module_host::approvals::native_http::HyperJsonTransport::from_env(&env,hook_proxy.as_ref()))).await.map_err(|error|{
                                let mut runtime=RuntimeError::new(error.message.clone());
                                if let Some(module)=error.module_error {runtime=runtime.with_js_error(crate::runtime::project::RuntimeJsError{name:"Error".into(),code:module.code.map(|s|String::from_utf16_lossy(s.units())),message:module.message,requires_host:false});}
                                runtime
                            })?;
                            Some(match sop_authority { Some(authority) => controller.with_knowledge_authority(authority, config.get("cwd").and_then(JsValue::as_string).cloned().unwrap_or_else(|| options.project.to_string_lossy().as_ref().into())), None => controller })
                        },
                        None=>None,
                    }
                }else{None};
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
                                let sdk_models=crate::runtime::turn::admission::SubmitSdkSessionConfig::models(sdk.as_ref(),&session,None)
                                    .await.map_err(|error|RuntimeError::new(error.message))?;
                                let sdk_permission_mode=sdk.permission_mode(&session).await;
                                let can_prompt=Some(seed.as_ref().map(|authority|authority.can_prompt.unwrap_or(true)).unwrap_or(false));
                                let policy_can_prompt=(seed.as_ref().is_some_and(|authority|authority.policy_can_prompt==Some(false))
                                    || config.as_ref().and_then(|config|config.get("managedPermissions")).and_then(|permissions|permissions.get("canPrompt"))==Some(&JsValue::Bool(false))).then_some(false);
                                Ok(Some(crate::runtime::turn::interaction::TurnInteractionAuthority{
                                    permission_mode:None,base_permission_mode:None,can_prompt,policy_can_prompt,
                                    sdk_permission_mode,sdk_hooks,sdk_models,emit,
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
                                    .or_else(|| session_overrides.automation(&session).map(|value| value.permission_mode))
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
                        .with_user_dialog(native_dialog)
                        .with_session_emitter(session_emit)
                        .with_pinned_steer(pinned_steer)
                        .with_permission_mode_source(permission_source.clone())
                        .with_permission_rules(native_permission_rules)
                        .with_automation(automation)
                        .with_web(native_web)
                        .with_knowledge(native_knowledge)
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
                        let ports = Arc::new(ports);
                        let factory=TurnHostFactory::new_with_async_authority(
                            project,
                            deployment,
                            native_host_limits(options.drain_budget),
                            Some("pilotdeck-agent-loop".into()),
                            move |identity, recorder, signal, authority| {
                                let ports = ports.clone();
                                let identity = identity.clone();
                                Box::pin(async move {
                                    let settings = match ports.persisted_permissions_home() {
                                        Some(home) => super::persisted_permissions::read_async(home).await?,
                                        None => super::persisted_permissions::Settings::default(),
                                    };
                                    Ok(ports.turn_with_authority_and_settings(&identity, recorder, signal, authority, settings))
                                })
                            },
                        )
                        .with_pinned_generation(pinned_generation)
                        .with_input_admission(input_admission)
                        .with_native_terminal_errors()
                        .with_agent_config(config)
                        // #1864 (b): TS re-reads the permission-mode override
                        // on every run (InProcessGateway.ts:1443), so an
                        // already-live session picks the change up without a
                        // close. The source reads the same store the selector
                        // method writes.
                        .with_permission_mode_source(permission_source);
                        Ok(match sop_controller {Some(controller)=>factory.with_loop_controller(controller),None=>factory})
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
                        let recording = persistence
                            .create_session_bounded(context.session_key.clone(), Some(open_path.clone()))?;
                        Ok(recording)
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
        // createLocalGateway.ts: the host preview is process-local temporary
        // output, separate from persistent session tool-result references.
        let artifacts = Arc::new(GatewayToolResultArtifactStore::new(
            crate::runtime::turn::projector::node_temp_dir(|key| std::env::var_os(key))?
                .join("pilotdeck-tool-output")
                .join(std::process::id().to_string()),
            4096,
        )?);
        let projector = Arc::new(GatewayAgentEventProjector::new(
            GatewayAgentEventProjectorOptions {
                tool_result_artifacts: Some(artifacts),
                ..Default::default()
            },
        )?);
        // One store keeps upload identities/leases; project admission resolves
        // through the same bounded registered-project metadata as dialog IO.
        let upload_store = super::project_scope::upload_store(options.home.clone());
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
    let temporary_finished: Arc<std::sync::atomic::AtomicBool> = Arc::default();
    let fallback_temporary_finished = temporary_finished.clone();
    let fallback_factory_finished = factory_finished.clone();
    let fallback_project_finished = project_finished.clone();
    let fallback_observer = cleanup_observer.clone();
    let build_factory = factory.clone();
    let build_project = project.clone();
    // #1328 A8.3: the channel assembly (ChannelAdapterBundle weixin subset
    // on the #1327 ChannelServer). The same config snapshot drives the
    // `adapters.weixin.enabled` startup gate and the ChannelStartDeps config;
    // #1326 takes the `cron` section off the same load.
    let (channel_assembly, automation_config) = {
        use crate::runtime::project::ConfigSource;
        let config = crate::runtime::project::NativeConfigSource::new(
            options.project.clone(),
            env.clone(),
            Some(options.home.clone()),
            None,
        )
        .bounded()
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
        // bundle constructors separately from the loader table.
        let pending_keys = crate::channel::loader::UNMIGRATED_CHANNEL_KEYS;
        let bundle_keys = super::channels::IMPLEMENTED_BUNDLE_CHANNEL_KEYS;
        #[cfg(test)]
        let (pending_keys, bundle_keys) = channel_declarations
            .as_ref()
            .map(|declarations| (declarations.unmigrated, declarations.bundle))
            .unwrap_or((pending_keys, bundle_keys));
        let rejected = if mode == AssemblyMode::Server {
            crate::channel::loader::reject_unmigrated_with_keys(
                config.get("adapters"),
                pending_keys,
            )
            .and_then(|()| {
                crate::channel::loader::reject_unimplemented_bundle_channels(
                    config.get("adapters"),
                    bundle_keys,
                )
            })
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
        let automation_config = automation::ProjectAutomationConfig {
            cron: (mode == AssemblyMode::Server)
                .then(|| config.get("cron").cloned())
                .flatten(),
            always_on: (mode == AssemblyMode::Server)
                .then(|| {
                    config
                        .get("alwaysOn")
                        .map(crate::runtime::always_on::AlwaysOnConfig::from_section)
                })
                .flatten(),
        };
        #[cfg(all(test, unix))]
        let channel_system_cli = channel_endpoints.channel_system_cli.clone();
        let channel_assembly = Arc::new(super::channels::NativeChannels::new(
            super::channels::NativeChannelsOptions {
                #[cfg(test)]
                endpoints: channel_endpoints,
                pilot_home: options.home.clone(),
                config: Arc::new(config),
                weixin: crate::channel::weixin::WeixinChannelOptions {
                    credentials_path: options.home.join("weixin-credentials.json"),
                    credentials_dir: Some(options.home.clone()),
                    ..Default::default()
                },
                weixin_maker: None,
                feishu: crate::channel::feishu::FeishuOptions::default(),
                // #2301: TS `consoleLogger` (pilotdeckServer.ts:67-72) — the
                // channel log the loader-table adapters' visible start lines
                // (`api-server: listening on …`, `sms: twilio not installed…`)
                // and the lifecycle core's startup lines go through.
                log: Some(super::channels::console_channel_log()),
                report_status: None,
                close_host: None,
            },
        ));
        let _ = channel_assembly
            .search
            .set(Arc::new(super::channel_search::NativeChannelSearch {
                home: options.home.clone(),
                cwd: options.project.clone(),
            }));
        #[cfg(unix)]
        {
            let executable = std::env::current_exe()?;
            let cli = executable
                .parent()
                .ok_or_else(|| {
                    std::io::Error::other("Native executable has no installation directory")
                })?
                .join("pilotdeck");
            #[cfg(test)]
            let cli = channel_system_cli.unwrap_or(cli);
            let mut channel_env = env.clone();
            channel_env.insert(
                "PILOT_HOME".into(),
                options.home.to_string_lossy().into_owned(),
            );
            let _ = channel_assembly.system.set(Arc::new(
                super::channel_system::NativeChannelSystem::new(
                    options.project.clone(),
                    cli,
                    options.token_file.clone(),
                    channel_env,
                ),
            ));
        }
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
        (channel_assembly, automation_config)
    };
    let automation_home = options.home.clone();
    let automation_project = project.clone();
    let automation_env = env.clone();
    let automation_frame_bytes = native_host_limits(options.drain_budget)
        .max_frame_bytes
        .min(crate::native_ws::MAX_OUTPUT_PAYLOAD);
    let automation_broadcast = turn_broadcast.clone();
    let automation_worktrees = worktree_broadcast.clone();
    let automation_bundle = crate::runtime::always_on::storage::run(move || {
        automation::ProjectAutomationBundle::new(automation::ProjectAutomationBundleOptions {
            config: automation_config,
            pilot_home: automation_home,
            make_always_on: always_on::native_factory(
                automation_project,
                automation_env,
                automation_frame_bytes,
                automation_broadcast,
                automation_worktrees,
            ),
            make_cron: cron::native_factory(),
        })
        .map(Arc::new)
    })
    .await;
    let automation_bundle = match automation_bundle {
        Ok(bundle) => bundle,
        Err(error) => {
            let _ = factory.dispose().await;
            let _ = project.dispose().await;
            return Err(error.into());
        }
    };
    let automation_lifetime =
        automation_lifetime::NativeAutomationLifetime::new(automation_bundle.clone());
    if let Some(slots) = &switch_slots {
        let stopping = automation_lifetime.clone();
        let stop: super::switch_wiring::AutomationStop = Arc::new(move || {
            let future = stopping.stop();
            Box::pin(async move { future.await.map_err(crate::switch::StepFailure::step) })
        });
        slots
            .automation
            .set(stop)
            .map_err(|_| std::io::Error::other("Automation stop slot already occupied."))?;
    }
    let published_automation = Arc::new(automation::PublishedAutomation::default());
    let build_automation = automation_lifetime.clone();
    let initialize_automation = automation_lifetime.clone();
    let cleanup_automation = automation_lifetime.clone();
    let build_channels = channel_assembly.clone();
    let cleanup_channels = channel_assembly.clone();
    // The generation lifetime owns automation start/reload/stop. This
    // compatibility handle lets switch shutdown drain the initial cron manager.
    let build_cron_slot: Arc<OnceLock<Arc<crate::switch::CronStopHandle>>> = match &switch_slots {
        Some(slots) => slots.cron.clone(),
        None => Arc::default(),
    };
    let cleanup_cron_slot = build_cron_slot.clone();
    // RefreshBundle::attach deliberately owns a strong config-store listener.
    // The host must detach it even when initialization or cleanup is cancelled.
    let refresh_cleanup = NativeRefreshCleanup(Arc::default());
    let publish_refresh = refresh_cleanup.0.clone();
    let lookup_forks = fork_registry.clone();
    let budget = options.drain_budget;
    let host = TurnGateway::start_native_with_initializer(
        TurnReplayOptions::new(),
        // #1746: the failure path waits out this embedding's own drain budget
        // (the cleanup below bounds itself by the same value), so a slow but
        // legal `--drain-ms` teardown is not cut by the host's default bound.
        budget,
        move |coordinator| {
            session_events.install(&coordinator);
            let owner = SessionOwner::new(SessionOwnerOptions::new(build_factory));
            owner.enforce_project_binding();
            ephemeral
                .bind_shutdown(Arc::new(owner.weak_shutdown()))
                .map_err(|error| std::io::Error::other(error.to_string()))?;
            lookup_forks.install_session_lookup(owner.weak_handle_lookup());
            lookup_forks.install_async_turn_lookup(owner.weak_async_turn_lookup());
            #[cfg(unix)]
            let replacement_storage = super::session_history::storage(
                options.home.clone(),
                options.project.clone(),
                owner.clone(),
            );
            #[cfg(not(unix))]
            let replacement_storage = Default::default();
            let replacement = Arc::new(ReplacementCoordinator::new(
                replacement_storage,
                Arc::new(owner.clone()),
                budget.as_millis() as u64,
                None,
            ));
            let dialog_recovery = Arc::new(dialog_recovery::NativeDialogRecovery::new(
                options.home.clone(),
                options.project.clone(),
                sdk_port.clone(),
            ));
            let admission = SubmitAdmissionOptions {
                interactions: interactions.clone(),
                sdk_config: Some(sdk_port.clone()),
                task_budget: Some(Arc::new(task_budget::NativeTaskBudget {
                    ledger: budget_ledger.clone(),
                    sdk: sdk_port.clone(),
                    fallback: options.project.clone(),
                })),
                dialog_recovery: Some(dialog_recovery.clone()),
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
                sdk_port.clone(),
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
            if let Some(notifications) = &server_notifications {
                settings.bundle.bind_server(notifications.clone());
            }
            let config_reload = Arc::downgrade(&build_automation);
            let channels_reload = Arc::downgrade(&build_channels);
            publish_refresh
                .set(settings.bundle.clone())
                .map_err(|_| std::io::Error::other("Native settings cleanup was already bound."))?;
            settings.store.subscribe(Arc::new(move |event| {
                if mode != AssemblyMode::Server {
                    return;
                }
                let changed = |family: &str| {
                    event.changed_paths.iter().any(|path| {
                        path.to_utf8().ok().is_some_and(|path| {
                            path == family || path.starts_with(&format!("{family}."))
                        })
                    })
                };
                let always_on_changed = changed("alwaysOn");
                let cron_changed = changed("cron");
                let adapters_changed = mode == AssemblyMode::Server
                    && event.changed_paths.iter().any(|path| {
                        path.to_utf8()
                            .ok()
                            .is_some_and(|path| path.starts_with("adapters."))
                    });
                if !(always_on_changed || cron_changed || adapters_changed) {
                    return;
                }
                if let Some(owner) = config_reload.upgrade() {
                    let config = event.next_snapshot.get("config");
                    let channel_reload = if adapters_changed {
                        channels_reload.upgrade().map(|channels| {
                            (channels, config.cloned().unwrap_or(JsValue::Undefined))
                        })
                    } else {
                        None
                    };
                    let automation_reload = (always_on_changed || cron_changed).then(|| {
                        automation::ProjectAutomationReloadInput {
                            config: automation::ProjectAutomationConfig {
                                always_on: (mode == AssemblyMode::Server)
                                    .then(|| {
                                        config.and_then(|c| c.get("alwaysOn")).map(
                                            crate::runtime::always_on::AlwaysOnConfig::from_section,
                                        )
                                    })
                                    .flatten(),
                                cron: (mode == AssemblyMode::Server)
                                    .then(|| config.and_then(|c| c.get("cron")).cloned())
                                    .flatten(),
                            },
                            always_on_changed,
                            cron_changed,
                        }
                    });
                    owner.request_config_reload(channel_reload, automation_reload);
                }
            }));
            // #1326 A8 U8: the one `CronManager::new` of the native process.
            // `None` (no `cron:` section) keeps the unconfigured shape — the
            // six names stay out of capabilities and every call answers the
            // `requireCron` gate. A build failure fails the whole assembly
            // through the same `project.dispose()` fallback below.
            let cron = automation_bundle.cron_manager();
            build_automation.bind_runtime(tokio::runtime::Handle::current())?;
            let standalone_project = build_project.clone();
            let standalone_options = options.clone();
            let standalone_env = env.clone();
            let automation_overrides = session_overrides.clone();
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
                manager_browser: manager_browser.clone(),
                dialog_recovery: Some(dialog_recovery.clone()),
                sop: Some(sop.clone()),
                upload_gateway: upload_gateway.clone(),
                // The CLI `prepareWeixinLogin` callback (pilotdeck.ts:240-247):
                // hot-start the weixin channel on request, after the same
                // durable-state restore as a config-enabled start.
                prepare_weixin_login: (mode == AssemblyMode::Server)
                    .then(|| build_channels.prepare_login_callback()),
                cron: cron.clone(),
                automation: (mode == AssemblyMode::Server).then(|| published_automation.clone()),
                settings: settings.gateway,
                session_overrides,
                ephemeral: Some(ephemeral.clone()),
                clear_sdk_state: Some(Arc::new(move |session| {
                    let sdk = sdk_port.clone();
                    Box::pin(async move {
                        sdk.registry
                            .lock()
                            .await
                            .clear_session_state_async(&session)
                            .await
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
            let automation_body = Arc::new(always_on::NativeAlwaysOnGatewayBody::new(
                cron_owner.clone(),
                submitter.clone(),
                automation_overrides,
                tokio::runtime::Handle::current(),
                build_automation.shutdown.clone(),
            ));
            let standalone = always_on::standalone_factory(
                standalone_project,
                standalone_options,
                standalone_env,
                automation_body.clone(),
                turn_broadcast.clone(),
                worktree_broadcast.clone(),
            );
            let plain_cron = Arc::new(crate::runtime::cron::SessionAdmissionCronGateway::new(
                cron_admission.clone(),
                Arc::new(cron::NativeCronGatewayBody::new(
                    cron_owner.clone(),
                    submitter.clone(),
                    tokio::runtime::Handle::current(),
                )),
            ));
            let cron_submitter = submitter.clone();
            let cron_runtime = tokio::runtime::Handle::current();
            let cron_channels = build_channels.clone();
            let busy_owner = cron_owner.clone();
            // The bundle owns this callback; a strong lifetime capture would form
            // lifetime -> bundle -> attachment -> callback -> lifetime.
            let stopping_automation = Arc::downgrade(&build_automation);
            if mode == AssemblyMode::Server {
                automation_bundle.attach(automation::ProjectAutomationAttachment {
                    agent_gateway: automation::ProjectAutomationAgentGateway {
                        always_on: automation_body,
                        cron: plain_cron,
                        native_cron_bind: Some(Arc::new(move |manager| {
                            let body = Arc::new(cron::NativeCronGatewayBody::new(
                                cron_owner.clone(),
                                cron_submitter.clone(),
                                cron_runtime.clone(),
                            ));
                            let shutdown = body.shutdown_signal();
                            let recording = Arc::new(crate::switch::RecordingCronGateway::wrap(
                                Arc::new(crate::runtime::cron::SessionAdmissionCronGateway::new(
                                    cron_admission.clone(),
                                    body,
                                )),
                            ));
                            manager
                                .bind_agent_gateway(recording.clone())
                                .map_err(cron::cron_io_error)?;
                            let options = execution_options.clone();
                            manager
                                .set_execution_project_filter(Arc::new(move |project| {
                                    options.serves_project(project)
                                }))
                                .map_err(cron::cron_io_error)?;
                            cron::bind_callbacks(
                                &manager,
                                cron_channels.server().clone(),
                                cron_runtime.clone(),
                                shutdown.clone(),
                                turn_broadcast.clone(),
                            )
                            .map_err(cron::cron_io_error)?;
                            let stop = Arc::new(crate::switch::CronStopHandle::new_native(
                                manager,
                                recording.failures(),
                                Duration::from_secs(15),
                                Arc::new(move || shutdown.cancel()),
                            ));
                            // Initial compatibility handle; reload generations are owned
                            // and stopped through the complete automation lifetime.
                            let _ = build_cron_slot.set(stop.clone());
                            Ok(stop)
                        })),
                    },
                    is_project_busy: Some(Arc::new(move |project| {
                        busy_owner.has_active_user_turn(&project.into())
                    })),
                    update_subsystems: Arc::new(move |update| {
                        if stopping_automation
                            .upgrade()
                            .is_none_or(|owner| owner.is_stopping())
                        {
                            published_automation.clear();
                        } else {
                            published_automation.publish(update, standalone.clone());
                        }
                    }),
                })?;
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
            if !build_channels.set_control(&requests) {
                return Err(std::io::Error::other("channel control already published"));
            }
            if !build_channels.set_interactions(&interactions) {
                return Err(std::io::Error::other(
                    "channel interaction coordinator already published",
                ));
            }
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
            if mode == AssemblyMode::Server {
                initialize_automation.start().await
            } else {
                Ok(())
            }
        },
        async move {
            refresh_cleanup.detach();
            // #1746: the host awaits this future on its own failure path before
            // reporting the error, so the deadline lives here; the shutdown
            // drain still bounds it by the embedding's shutdown budget.
            let teardown = async move {
                // #2294 A7.8: stop the automation bundle first — automation
                // precedes closeServer (PilotDeckServerShutdownBundle.ts:33-37).
                // The bundle's stop order is cron first, then always-on, so
                // producer dependencies drain in reverse startup order; a stop
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
                let automation_stop = if mode == AssemblyMode::Server {
                    cleanup_observer("automation", cleanup_automation.stop().await).await
                } else {
                    Ok(())
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
                let temporary = cleanup_ephemeral.close().await.map_err(|e| e.to_string());
                temporary_finished.store(true, std::sync::atomic::Ordering::Release);
                let temporary = cleanup_observer("temporary sessions", temporary).await;
                if let Err(error) = &temporary {
                    cleanup_diagnostic(error);
                }
                let project = cleanup_project.dispose().await.map_err(|e| e.to_string());
                project_finished.store(true, std::sync::atomic::Ordering::Release);
                let project = cleanup_observer("project", project).await;
                if let Err(error) = &project {
                    cleanup_diagnostic(error);
                }
                let mut outcome = sessions.and(temporary).and(project);
                for failure in [
                    cron_stop.err(),
                    automation_stop.err(),
                    channel_stop.err(),
                    channel_flush.err(),
                ]
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
            if let Some(notifications) = notifications {
                notifications.bind(host.notification_server());
            }
            // #1328: start config-enabled channels (pilotdeckServer.ts:288-296)
            // once the builder has published the gateway port. #2301: the
            // configured half — `loadEnabledChannels(config.adapters)` — joins
            // the loop after the bundle keys, in the TS startup order
            // (feishu, weixin, qq, then `channels`;
            // `ChannelAdapterBundle.createStartupAdapters`).
            let mut startup = Vec::new();
            if mode == AssemblyMode::Server {
                if let Some(feishu) = channel_assembly.create_startup_feishu().await {
                    startup.push(feishu);
                }
                if let Some(weixin) = channel_assembly.create_startup_weixin().await {
                    startup.push(weixin);
                }
                if let Some(qq) = channel_assembly.create_startup_qq().await {
                    startup.push(qq);
                }
                if let Some(wecom) = channel_assembly.create_startup_wecom().await {
                    startup.push(wecom);
                }
                startup.extend(channel_assembly.create_startup_configured());
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
            if !fallback_temporary_finished.load(std::sync::atomic::Ordering::Acquire) {
                let _ = tokio::time::timeout(budget, async {
                    let result = fallback_ephemeral.close().await.map_err(|e| e.to_string());
                    fallback_temporary_finished.store(true, std::sync::atomic::Ordering::Release);
                    fallback_observer("temporary sessions", result).await
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
struct NativeRefreshCleanup(Arc<OnceLock<Arc<crate::runtime::refresh::RefreshBundle>>>);
impl NativeRefreshCleanup {
    fn detach(&self) {
        if let Some(bundle) = self.0.get() {
            bundle.dispose();
        }
    }
}
impl Drop for NativeRefreshCleanup {
    fn drop(&mut self) {
        self.detach();
    }
}
fn native_host_limits(drain_budget: std::time::Duration) -> crate::host::Limits {
    crate::host::Limits {
        close_timeout: drain_budget,
        ..Default::default()
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
        apply_output_style_content(config, content);
    }
}

/// Join style text without rewriting permission/thinking choices twice.
fn apply_output_style_content(config: &mut JsObject, content: &str) {
    let existing = config
        .get("appendSystemPrompt")
        .and_then(JsValue::as_string);
    let mut parts = Vec::new();
    if let Some(existing) = existing {
        let units = crate::runtime::memory::js_trim_units(existing.units());
        if !units.is_empty() {
            parts.push(existing.clone());
        }
    }
    if !crate::metadata::js_trim(content).is_empty() {
        parts.push(JsString::from(content));
    }
    let mut joined = Vec::new();
    for (index, part) in parts.iter().enumerate() {
        if index != 0 {
            joined.extend("\n\n".encode_utf16());
        }
        joined.extend_from_slice(part.units());
    }
    config.insert(
        "appendSystemPrompt".into(),
        if joined.is_empty() {
            JsValue::Undefined
        } else {
            JsValue::String(JsString::from_utf16(joined))
        },
    );
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
    // The native generation independently rechecks this restricted profile
    // before loading output styles, standalone skills, and configured hooks.
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
    .bounded()
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
/// U1 upload store `resolveProject`: the configured project only, accepted
/// under either spelling — the canonical path or the logical one the process
/// was configured with (the two differ when `--project` itself names a
/// symlink; the upload record keeps the canonical spelling, the client may
/// echo either). Any other key is `unknown_project_key` with the caller's raw
/// UTF-16 (#1441: extracted from the wiring closure so the dual-spelling
/// acceptance has a direct test).
pub(super) fn upload_resolve_project(
    upload_project: &std::path::Path,
    key: &JsString,
) -> std::result::Result<PathBuf, crate::upload::UploadError> {
    // Shared helper `Unknown projectKey: <raw UTF-16>` (UploadStore.ts:63,
    // #865 frozen behavior): the other 15+ call sites already report the
    // exact UTF-16 units.
    let path = match key.to_utf8() {
        Ok(utf8) => PathBuf::from(utf8),
        Err(_) => return Err(crate::upload::unknown_project_key(key)),
    };
    // Accept both the canonical path and the logical path.
    let canonical =
        std::fs::canonicalize(upload_project).unwrap_or_else(|_| upload_project.to_path_buf());
    let requested = std::fs::canonicalize(&path).unwrap_or(path.clone());
    if requested != canonical && path != upload_project {
        return Err(crate::upload::unknown_project_key(key));
    }
    Ok(upload_project.to_path_buf())
}

// These are the normalized native bindings from parseModulesConfig. In the
// TS runtime they retain the same default owner as an absent binding; only an
// external protocol binding replaces a model/tool/context/agent-loop port.
fn supports_native_module_binding(name: &JsString, value: &JsValue) -> bool {
    let Some(fields) = value.as_object() else {
        return false;
    };
    if name.equals_str("sop") {
        // parseModulesConfig has already checked endpoints, paths and contract.
        // The selected generation now installs the real StaffDeck controller.
        return fields
            .get("enabled")
            .is_none_or(|enabled| enabled == &JsValue::Bool(true))
            && (fields
                .get("provider")
                .and_then(JsValue::as_string)
                .is_some_and(|p| p.equals_str("staffdeck"))
                || (fields.get("contract") == Some(&JsValue::from("sop.lifecycle/v2"))
                    && fields.get("transport") == Some(&JsValue::from("sop-http-v2"))));
    }
    if name.equals_str("knowledge")
        && fields.get("transport") == Some(&JsValue::from("module-http-v2"))
        && crate::skills::ExternalSkillBinding::from_value(value).is_ok()
    {
        return true;
    }
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

#[cfg(test)]
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
        Arc::new(super::projects::NativeProjectFactory::new(
            options.clone(),
            env.clone(),
        )),
    ))
}

#[cfg(test)]
mod resolve_project_tests {
    use super::upload_resolve_project;
    use pilotdeck_protocol::JsString;

    /// #1441（探针 ts-probe/p4-resolve-project.out.txt）：resolveProject 收
    /// 规范拼写与逻辑拼写两种（Rust 装配闭包的超集行为——TS registry 是
    /// 词法等值匹配，差异如实记录在 RESULT），外来路径按原始 UTF-16 报
    /// `Unknown projectKey: <原样>`。
    #[test]
    fn upload_resolve_project_accepts_both_spellings() {
        let scratch =
            std::env::temp_dir().join(format!("pdrs-1441-resolve-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(scratch.join("real")).expect("#1441 scratch dir");
        let real = scratch
            .join("real")
            .canonicalize()
            .expect("#1441 canonical");
        let logical = scratch.join("alias");
        std::os::unix::fs::symlink(&real, &logical).expect("#1441 alias symlink");
        // 配置拼写 = 逻辑（经 symlink）
        let configured = &logical;
        for key in [&logical, &real] {
            let resolved =
                upload_resolve_project(configured, &JsString::from(key.to_string_lossy().as_ref()))
                    .expect("#1441 both spellings must resolve");
            assert_eq!(resolved, logical, "the registered spelling is returned");
        }
        // 外来路径：精确消息（原样回显）
        let foreign = scratch.join("nowhere");
        let error = upload_resolve_project(
            configured,
            &JsString::from(foreign.to_string_lossy().as_ref()),
        )
        .expect_err("#1441 a foreign key must be refused");
        assert_eq!(
            String::from_utf16_lossy(error.message.units()),
            format!("Unknown projectKey: {}", foreign.to_string_lossy()),
            "the refusal echoes the caller's spelling"
        );
        // 反向：配置拼写 = 规范时，逻辑拼写同样通过
        let resolved =
            upload_resolve_project(&real, &JsString::from(logical.to_string_lossy().as_ref()))
                .expect("#1441 the logical spelling resolves against a canonical config");
        assert_eq!(resolved, real);
        std::fs::remove_dir_all(&scratch).ok();
    }
}

/// Reuse the actual resource-family composition for each registry root. Boot
/// cwd/env/bundled assets remain process selections; project-owned extensions
/// use the root being staged, like the model/context/persistence stage context.
pub(super) fn project_resources_factory(
    options: &Options,
    env: &Env,
    project_root: &Path,
    background: Option<Arc<dyn crate::runtime::project::ResourceFamilySource>>,
) -> ProjectResourcesFactory {
    ProjectResourcesFactory::new(ProjectResourcesOptions {
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
        .with_native_models_from_env(env.clone())
        .with_context_memory()
        .with_tools_mcp()
        .with_shared_native_background_tasks(background)
        .with_native_style_plugins(crate::runtime::plugins::PluginRuntimeOptions {
            project_root: project_root.to_path_buf(),
            pilot_home: options.home.clone(),
            builtin_skills_root: explicit_builtin_skills_root(options, env),
            builtin_plugins: vec![],
            builtin_plugins_enabled: HashMap::new(),
        })
        .with_interaction(),
    })
    .with_bounded_config()
}

#[cfg(test)]
mod surrogate_style_regression {
    use super::*;
    #[test]
    fn output_style_composition_preserves_existing_utf16_units() {
        let mut config = JsObject::new();
        config.insert(
            "appendSystemPrompt".into(),
            JsValue::String(JsString::from_utf16(vec![65, 0xd800, 66])),
        );
        apply_session_overrides(&mut config, None, None, Some("  style  "));
        let value = config
            .get("appendSystemPrompt")
            .unwrap()
            .as_string()
            .unwrap();
        let mut expected = vec![65, 0xd800, 66];
        expected.extend("\n\n  style  ".encode_utf16());
        assert_eq!(value.units(), expected);
    }
}

#[cfg(test)]
mod native_refresh_cleanup_tests {
    use super::*;

    #[tokio::test]
    async fn real_config_refresh_detaches_on_explicit_cleanup_and_unpolled_future_drop() {
        for unpolled in [false, true] {
            let (_scratch, requests) = crate::native_process::management_tests::setup();
            let settings = crate::native_process::services::native_settings_for_process(
                requests.owner.clone(),
                requests.project.clone(),
                &requests.options.home,
                &requests.options.project,
                &requests.env,
            )
            .expect("real settings stack");
            let store = settings.store.clone();
            let weak = Arc::downgrade(&settings.bundle);
            assert_eq!(store.listener_count(), 1, "refresh attached");
            let cleanup = NativeRefreshCleanup(Arc::default());
            assert!(cleanup.0.set(settings.bundle.clone()).is_ok());
            drop(settings);
            assert!(
                weak.upgrade().is_some(),
                "listener and cleanup own the bundle before detach"
            );
            if unpolled {
                // Same ownership shape as a cleanup future dropped before its first poll.
                let future = async move {
                    cleanup.detach();
                };
                drop(future);
            } else {
                cleanup.detach();
                cleanup.detach(); // explicit shutdown and Drop must be idempotent
                drop(cleanup);
            }
            assert_eq!(
                store.listener_count(),
                0,
                "real ConfigStore listener removed"
            );
            assert!(weak.upgrade().is_none(), "bundle and owner graph released");
            requests.owner.shutdown().await.unwrap();
            requests.project.dispose().await.unwrap();
        }
    }
}
