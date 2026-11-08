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
            plugins::PluginResources,
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
            host_factory::{Deployment, TurnHostFactory},
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
    path::PathBuf,
    sync::{Arc, Mutex, OnceLock, Weak},
    time::{Duration, SystemTime},
};

mod cron;
mod mcp;
mod memory;
pub(crate) mod snapshot;
mod steer;
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

/// Inputs the assembly normally creates itself and only tests vary. Production
/// always uses `Default`: the rule-set registry is created in place, the MCP
/// config is published, and an acquire miss goes to stderr.
pub(super) struct AssemblySeams {
    pub permission_rule_sets: Option<Arc<GatewaySessionPermissionRuleSetRegistry>>,
    pub acquire_miss_report: Option<AcquireMissReport>,
    pub publish_mcp_config: bool,
}

impl Default for AssemblySeams {
    fn default() -> Self {
        Self {
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
            permission_rule_sets: Some(permission_rule_sets),
            acquire_miss_report: Some(miss_report),
            publish_mcp_config,
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
    register_project(&options)?;
    let project = project_registry(&options, &env);
    let snapshot_enabled = snapshot::snapshot_enabled(&options, &project).await?;
    let snapshot_recorder =
        snapshot::snapshot_recorder(&options, &project, snapshot_enabled).await?;
    let memory = memory::build_memory(options.clone(), project.clone()).await?;
    let recorders: Recorders = Arc::default();
    let permission_rule_sets = permission_rule_sets.unwrap_or_default();
    let interactions = Arc::new(GatewayInteractionCoordinator::new(InteractionOptions {
        permission_grants: Some(permission_rule_sets.clone()),
        ..InteractionOptions::default()
    }));
    let user_dialog_bus = user_dialog::build_user_dialog_bus();
    // #1864: the per-gateway session selection store. One instance serves
    // both writers (the three selector methods on `Requests`) and the reader
    // (the prepare closure composing each session's agent config).
    let session_overrides: Arc<super::SessionOverrides> = Arc::default();
    let prepare_overrides = session_overrides.clone();
    let prepare_project = project.clone();
    let prepare_options = options.clone();
    let prepare_env = env.clone();
    let prepare_recorders = recorders.clone();
    let prepare_interactions = interactions.clone();
    let prepare_permission_rule_sets = permission_rule_sets.clone();
    // Forward reference so the prepare closure can call sync_permission_rules
    // after lease acquire (mirrors ProjectRuntimeRegistry.ts:305-308).
    let prepare_mcp_config: Arc<std::sync::OnceLock<Arc<SdkMcpConfig>>> = Arc::default();
    // #1429: report at most once per assembly — a per-acquire stderr write would
    // be unbounded if the publication regressed for good.
    let acquire_miss_report: AcquireMissReport =
        acquire_miss_report.unwrap_or_else(|| Arc::new(|message: &str| eprintln!("{message}")));
    let prepare_acquire_miss_report = acquire_miss_report.clone();
    let prepare_acquire_miss_reported: Arc<std::sync::OnceLock<()>> = Arc::default();
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
    let factory = Arc::new(ProjectSessionFactory::new(FactoryOptions {
        writer_busy_wait_timeout: options.drain_budget,
        prepare: Arc::new(move |context, _| {
            let project = prepare_project.clone();
            let options = prepare_options.clone();
            let env = prepare_env.clone();
            let recorders = prepare_recorders.clone();
            let interactions = prepare_interactions.clone();
            let permission_rule_sets = prepare_permission_rule_sets.clone();
            let mcp_config_handle = prepare_mcp_config.clone();
            let acquire_miss_report = prepare_acquire_miss_report.clone();
            let acquire_miss_reported = prepare_acquire_miss_reported.clone();
            let session_overrides = prepare_overrides.clone();
            Box::pin(async move {
                let lease = project.acquire(None)?;
                let resources = lease
                    .runtime()
                    .resources()
                    .get::<ProjectResources>()
                    .ok_or_else(|| RuntimeError::new("Native project resources are missing."))?;
                let staged = resources.context();
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
                let snapshot = staged.snapshot();
                let agent = snapshot
                    .get("config")
                    .and_then(|v| v.get("agent"))
                    .cloned()
                    .unwrap_or(JsValue::Undefined);
                let mut config = agent.as_object().cloned().unwrap_or_default();
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
                // Preserve actual configured ports before the Kernel whitelist.
                // Merely having an optional family installed is not activation.
                let kernel_pricing = staged
                    .resource(Resource::Router)
                    .and_then(|resource| {
                        resource
                            .as_any()
                            .downcast_ref::<crate::runtime::project::model::RouterResources>()
                            .map(|router| router.router.config().stats.model_pricing.clone())
                    })
                    .flatten()
                    .map(|pricing| {
                        JsValue::Array(
                            pricing
                                .iter()
                                .map(|(key, rate)| {
                                    crate::metadata::object([
                                        ("key", JsValue::from(key)),
                                        (
                                            "input",
                                            rate.input
                                                .map(JsValue::Number)
                                                .unwrap_or(JsValue::Null),
                                        ),
                                        (
                                            "output",
                                            rate.output
                                                .map(JsValue::Number)
                                                .unwrap_or(JsValue::Null),
                                        ),
                                        (
                                            "cacheRead",
                                            rate.cache_read
                                                .map(JsValue::Number)
                                                .unwrap_or(JsValue::Null),
                                        ),
                                    ])
                                })
                                .collect(),
                        )
                    });
                let memory_active = snapshot
                    .get("config")
                    .and_then(|config| config.get("memory"))
                    .and_then(|memory| memory.get("enabled"))
                    == Some(&JsValue::Bool(true));
                let instruction = crate::runtime::context::InstructionDiscovery::new(
                    options.project.clone(),
                    options.project.clone(),
                    options.home.clone(),
                    env.get("PILOTDECK_MANAGED_CONFIG")
                        .filter(|path| !path.is_empty())
                        .map(PathBuf::from),
                );
                let plugins = staged
                    .resource(Resource::PluginRuntime)
                    .and_then(|resource| {
                        resource
                            .as_any()
                            .downcast_ref::<PluginResources>()
                            .map(|plugins| plugins.runtime.clone())
                    });
                // Run only after this session's immutable owner selects Kernel;
                // a legacy preparation never executes or refreshes these ports.
                let kernel_port_preflight: crate::runtime::refresh::BoxFuture<
                    crate::runtime::project::RuntimeResult,
                > = Box::pin(async move {
                    if memory_active {
                        return Err(crate::runtime::session::kernel_config::unavailable(
                            "resolved product memory resolver",
                        ));
                    }
                    let layers = tokio::task::spawn_blocking(move || instruction.discover())
                        .await
                        .map_err(|error| RuntimeError::new(error.to_string()))?
                        .map_err(RuntimeError::new)?;
                    if !layers.is_empty() {
                        return Err(crate::runtime::session::kernel_config::unavailable(
                            "discovered PILOTDECK.md/rules instructions",
                        ));
                    }
                    if let Some(plugins) = plugins {
                        let plugins = plugins
                            .refresh()
                            .await
                            .map_err(|error| RuntimeError::new(error.to_string()))?;
                        if plugins.iter().any(|plugin| {
                            !plugin.commands.is_empty()
                                || !plugin.skills.is_empty()
                                || plugin.hooks_config.is_some()
                                || plugin.mcp_servers.is_some()
                                || plugin.lsp_servers.is_some()
                        }) {
                            return Err(crate::runtime::session::kernel_config::unavailable(
                                "active product plugin contributions",
                            ));
                        }
                    }
                    Ok(())
                });
                let mut retained = PreparedResources::default();
                retained.retain_lease(lease);
                let permission_lease = permission_rule_sets.acquire(&context.session_key, None);
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
                let tail = TailConfig {
                    cwd: Some(options.project.clone()),
                    // #1371: the controlled loop emits no assistant text, so
                    // the automatic title request is a stimulus difference
                    // (RESULT.md TEXT row). An armed loop *with a plan*
                    // removes that side effect instead of leaving a
                    // non-comparable extra provider request in the recorded
                    // cells; an inert entry keeps the production request.
                    auto_generate_title: !loop_planned,
                    title_provider: (!loop_planned).then_some(title),
                    ..Default::default()
                };
                let session = context.session_key;
                Ok(PreparedSession {
                    resources: retained,
                    tail,
                    host: Box::new(move |binding| {
                        // #1864 (b): one source closure feeds both the session
                        // tools and the live adapter — the same store and key
                        // the prepare closure read back.
                        let permission_source = {
                            let session_overrides = session_overrides.clone();
                            let session = session.clone();
                            Arc::new(move || session_overrides.permission_mode(&session))
                        };
                        recorders
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .insert(session.clone(), Arc::downgrade(&binding.recorder));
                        #[cfg(unix)]
                        if let crate::runtime::session::kernel::BackendOwner::Kernel(factory) =
                            crate::runtime::session::kernel::KernelBackendFactory::select(
                                &binding,
                                &env,
                                &options.project,
                            )?
                        {
                            let allowed=binding.context.allowed_tools.clone();
                            let denied=binding.context.disallowed_tools.clone();
                            let kernel_port_preflight:crate::runtime::refresh::BoxFuture<crate::runtime::project::RuntimeResult>=Box::pin(async move {
                                crate::runtime::session::kernel_config::session_tool_filters(allowed.as_deref(),denied.as_deref())?;
                                kernel_port_preflight.await
                            });
                            let mut kernel_config = config.clone();
                            kernel_config.insert(
                                "modelPricingEntries".into(),
                                kernel_pricing.unwrap_or(JsValue::Undefined),
                            );
                            return Ok(Box::new(
                                factory
                                    .with_port_preflight(kernel_port_preflight)
                                    .with_interactions(interactions.clone())
                                    .with_turn_preferences(
                                        kernel_config,
                                        permission_source.clone(),
                                    ),
                            ));
                        }
                        if options.sidecar.as_os_str().is_empty() {
                            return Err(RuntimeError::new(
                                "This session belongs to the legacy backend; configure --sidecar to open it.",
                            ));
                        }
                        recorders
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .insert(session.clone(), Arc::downgrade(&binding.recorder));
                        let deployment = Deployment::Stdio {
                            command: options.sidecar.to_string_lossy().into_owned(),
                            args: options.sidecar_args.clone(),
                            cwd: options.project.clone(),
                            env: env.clone(),
                        };
                        let ports = super::tools::SessionTools::new(
                            &options,
                            &env,
                            binding,
                            config.clone(),
                            interactions,
                            lsp,
                        )?
                        .with_permission_mode_source(permission_source.clone());
                        Ok(Box::new(
                            TurnHostFactory::new_with_signal(
                                project,
                                deployment,
                                crate::host::Limits {
                                    close_timeout: options.drain_budget,
                                    ..Default::default()
                                },
                                Some("pilotdeck-agent-loop".into()),
                                move |identity, recorder, signal| {
                                    turn_ports(&ports, identity, recorder, signal)
                                },
                            )
                            .with_agent_config(config)
                            // #1864 (b): TS re-reads the permission-mode override
                            // on every run (InProcessGateway.ts:1443), so an
                            // already-live session picks the change up without a
                            // close. The source reads the same store the selector
                            // method writes.
                            .with_permission_mode_source(permission_source),
                        ))
                    }),
                })
            })
        }),
        create_storage: Arc::new(move |context| {
            let project = storage_project.clone();
            let options = storage_options.clone();
            Box::pin(async move {
                let lease = Arc::new(project.acquire(None)?);
                let resource = lease
                    .runtime()
                    .resources()
                    .get::<ProjectResources>()
                    .ok_or_else(|| RuntimeError::new("Native persistence project is missing."))?
                    .context()
                    .resource(Resource::Persistence)
                    .ok_or_else(|| RuntimeError::new("Native persistence is missing."))?;
                let path = transcript_path(&options, &context.session_key);
                let open_path = path.clone();
                Ok(SessionStorage::Recording(RecordingPlan {
                    transcript_path: path,
                    open: Arc::new(move || {
                        let _lease = &lease;
                        let persistence = resource
                            .as_any()
                            .downcast_ref::<PersistenceResources>()
                            .ok_or_else(|| {
                                crate::storage::recorder::RecorderError::Type(
                                    "Native persistence has the wrong type.".into(),
                                )
                            })?;
                        persistence
                            .create_session(context.session_key.clone(), Some(open_path.clone()))
                    }),
                }))
            })
        }),
        admission: None,
        tool_results_dir: Some(Arc::new(move |session_id| {
            super::tools::session_tool_results_dir(&budget_project, session_id)
        })),
        diagnostic: Arc::new(|d| eprintln!("native session: {d:?}")),
    }));
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
    crate::upload::spawn_cleanup(&upload_store, upload_cleanup_interval);
    let cleanup_factory = factory.clone();
    let cleanup_project = project.clone();
    // #1746: exactly one teardown may run. The host polls this cleanup itself
    // when initialization fails — `start_with_cleanup_budget` below hands it
    // this embedding's `budget`, so the wait is never shorter than the timeout
    // inside this closure — and the fallback below arms only when the host
    // thread never started (runtime build or spawn failure), leaving the
    // future dropped unpolled.
    let cleanup_ran: Arc<std::sync::atomic::AtomicBool> = Arc::default();
    let cleanup_fallback_ran = cleanup_ran.clone();
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
        let rejected = crate::channel::loader::reject_unmigrated_channels(config.get("adapters"))
            .and_then(|()| {
                crate::channel::loader::reject_unimplemented_bundle_channels(
                    config.get("adapters"),
                    super::channels::IMPLEMENTED_BUNDLE_CHANNEL_KEYS,
                )
            });
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
        let cron_section = config.get("cron").cloned();
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
    let budget = options.drain_budget;
    let host = TurnGateway::start_with_cleanup_budget(
        TurnReplayOptions::new(),
        // #1746: the failure path waits out this embedding's own drain budget
        // (the cleanup below bounds itself by the same value), so a slow but
        // legal `--drain-ms` teardown is not cut by the host's default bound.
        budget,
        move |coordinator| {
            let owner = SessionOwner::new(SessionOwnerOptions::new(build_factory));
            let replacement = Arc::new(ReplacementCoordinator::new(
                Default::default(),
                Arc::new(owner.clone()),
                budget.as_millis() as u64,
                None,
            ));
            let admission = SubmitAdmissionOptions {
                #[cfg(unix)]
                compaction: Some(Arc::new(super::compaction::NativeCompactionRouter { owner:owner.clone(),options:options.clone() })),
                interactions: interactions.clone(),
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
            );
            let async_hook_registry =
                steer::build_async_hook_registry(&build_project, &owner, &coordinator);
            // U7 global settings (#1320): `createPilotConfigStoreSync` +
            // `runtimeRefresh` + the settings gateway
            // (createLocalGateway.ts:795-835, 1383-1410), composed in
            // `services::native_settings_for_process`; a store load failure
            // fails assembly, like the TS boot.
            let settings = super::services::native_settings_for_process(
                owner.clone(),
                build_project.clone(),
                &options.home,
                &options.project,
                &env,
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
                prepare_weixin_login: Some(build_channels.prepare_login_callback()),
                cron: cron.clone(),
                settings: settings.gateway,
                session_overrides,
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
                cron.start().map_err(cron::cron_io_error)?;
                build_cron_slot
                    .set(Arc::new(crate::switch::CronStopHandle::new(
                        cron.clone(),
                        recording.failures(),
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
            if cleanup_ran.swap(true, std::sync::atomic::Ordering::SeqCst) {
                return Ok(());
            }
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
                                .stop_once()
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
                let sessions = cleanup_observer(
                    "session",
                    cleanup_factory.dispose().await.map_err(|e| e.to_string()),
                )
                .await;
                let project = cleanup_observer(
                    "project",
                    cleanup_project.dispose().await.map_err(|e| e.to_string()),
                )
                .await;
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
            if let Some(feishu) = channel_assembly.create_startup_feishu().await {
                startup.push(feishu);
            }
            if let Some(weixin) = channel_assembly.create_startup_weixin().await {
                startup.push(weixin);
            }
            if !startup.is_empty() {
                channel_assembly.server().start_channels(startup);
            }
            Ok(host)
        }
        Err(error) => {
            // #1746: the host disposes the coordinator and polls the cleanup
            // above before reporting a builder failure. This fallback owns the
            // project/factory teardown only when the host thread never started
            // — running it twice would dispose the same families twice.
            if !cleanup_fallback_ran.swap(true, std::sync::atomic::Ordering::SeqCst) {
                let _ = tokio::time::timeout(budget, async {
                    let _ = factory.dispose().await;
                    project.dispose().await
                })
                .await;
            }
            Err(error.into())
        }
    }
}
pub fn transcript_path(options: &Options, session: &JsString) -> std::path::PathBuf {
    // #1860: one derivation, shared with the transcript write fence — a second
    // copy could drift and leave the fence guarding a file the store never
    // opens.
    crate::runtime::session::transcript_locks::session_transcript_path(
        &options.home,
        &options.project,
        session,
    )
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
    let chats = crate::session_chat_dir(
        &options.home,
        &options.project,
        &options.project.to_string_lossy(),
    );
    let directory = chats
        .parent()
        .ok_or("Native project storage has no parent.")?;
    std::fs::create_dir_all(directory)?;
    let marker = directory.join(".cwd");
    if marker.exists() {
        let existing = std::fs::read_to_string(&marker)?;
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
    management: bool,
) -> Result<(), ProxyError> {
    struct Captured<'a>(&'a Env, &'a std::path::Path);
    impl crate::runtime::mcp::protocol::McpEnvironment for Captured<'_> {
        fn env(&self, name: &JsString) -> Option<JsString> {
            self.0.get(&name.to_utf8().ok()?).map(|v| v.as_str().into())
        }
        fn home(&self) -> JsString {
            self.1.to_string_lossy().into_owned().into()
        }
    }
    let mcp = crate::runtime::mcp::protocol::load_mcp_server_config(
        &options.project,
        &options.home,
        &Captured(env, &options.home),
    )?;
    if !mcp.servers.own_entries().is_empty() || !mcp.diagnostics.is_empty() {
        return Err("Native isolated profile does not yet support configured MCP servers.".into());
    }
    for base in [&options.home, &options.project.join(".pilotdeck")] {
        for name in ["plugins", "skills"] {
            if management && name == "skills" {
                continue;
            }
            match std::fs::read_dir(base.join(name)) {
                Ok(entries) => {
                    if entries.count() != 0 {
                        return Err("Native isolated profile does not yet support configured plugins, skills or hooks.".into());
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
        }
    }
    use crate::runtime::project::ConfigSource;
    let snapshot = crate::runtime::project::NativeConfigSource::new(
        options.project.clone(),
        env.clone(),
        Some(options.home.clone()),
        None,
    )
    .load(&options.project)?;
    // The fixed config loader drops unknown top-level fields from config,
    // so inspecting config.hooks alone silently misses the supplied YAML.
    // Plugin hooks are refused above; SDK hooks are refused by workspace_id.
    if snapshot
        .get("diagnostics")
        .and_then(|value| match value {
            JsValue::Array(rows) => Some(rows),
            _ => None,
        })
        .is_some_and(|rows| {
            rows.iter()
                .any(|row| row.get("path") == Some(&"hooks".into()))
        })
    {
        return Err("Native isolated profile does not yet support configured hooks.".into());
    }
    if snapshot
        .get("config")
        .and_then(|v| v.get("modules"))
        .and_then(JsValue::as_object)
        .is_some_and(|modules| !modules.own_entries().is_empty())
    {
        return Err(
            "Native isolated profile does not yet support configured module bindings.".into(),
        );
    }
    if snapshot
        .get("config")
        .and_then(|v| v.get("agent"))
        .and_then(|v| v.get("subagents"))
        .is_some_and(|v| !v.is_undefined())
    {
        return Err("Native isolated profile does not yet support configured subagents.".into());
    }
    Ok(())
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

pub(super) fn project_registry(options: &Options, env: &Env) -> Arc<ProjectRuntimeRegistry> {
    Arc::new(ProjectRuntimeRegistry::new(
        options.project.clone(),
        Arc::new(ProjectResourcesFactory::new(ProjectResourcesOptions {
            pilot_home: options.home.clone(),
            cwd: options.project.clone(),
            env: env.clone(),
            homedir: Some(options.home.clone()),
            builtin_skills_root: None,
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
            .with_plugins(crate::runtime::plugins::PluginRuntimeOptions {
                project_root: options.project.clone(),
                pilot_home: options.home.clone(),
                builtin_skills_root: None,
                builtin_plugins: vec![],
                builtin_plugins_enabled: HashMap::new(),
            })
            .with_interaction(),
        })),
    ))
}
