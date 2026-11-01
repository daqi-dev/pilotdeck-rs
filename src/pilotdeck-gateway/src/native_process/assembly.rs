use super::{
    Options,
    families::{LspFamily, TitleFamily, TitleResource},
};
use crate::{
    ProxyError,
    config::Env,
    metadata::object,
    runtime::{
        interaction::GatewayInteractionCoordinator,
        project::{
            Families, Family, ProjectResources, ProjectResourcesFactory, ProjectResourcesOptions,
            ProjectRuntimeRegistry, Resource, RuntimeError, RuntimeProfileOverrides,
            persistence::PersistenceResources,
        },
        replacement::ReplacementCoordinator,
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
            publish::FileRegistry,
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
    sync::{Arc, Mutex, Weak},
    time::SystemTime,
};

pub type Recorders = Arc<Mutex<HashMap<JsString, Weak<Recorder>>>>;

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

// An observer runs only after the actual production disposal. Tests can inject
// a failure or a deterministic completion gate without substituting resources.
pub(super) async fn start_observed(
    options: Options,
    env: Env,
    cleanup_observer: CleanupObserver,
    cleanup_diagnostic: Arc<dyn Fn(&str) + Send + Sync>,
) -> Result<TurnGateway, ProxyError> {
    // Management can inspect existing skills; turn admission still checks that
    // unsupported skill prompt/tool composition is not silently omitted.
    validate_profile_inner(&options, &env, true).await?;
    let mut env = env;
    let storage = env.entry("PILOTDECK_STORAGE_ROOT".into()).or_default();
    if storage.is_empty() {
        *storage = options.home.join("storage").to_string_lossy().into_owned();
    }
    register_project(&options)?;
    let project = project_registry(&options, &env);
    // Validate real families before claiming readiness. Partial staging belongs
    // to this registry and must drain even when construction returns an error.
    if let Err(error) = project.acquire(None) {
        let _ = tokio::time::timeout(options.drain_budget, project.dispose()).await;
        return Err(error.into());
    }
    let recorders: Recorders = Arc::default();
    let interactions = Arc::new(GatewayInteractionCoordinator::default());
    let prepare_project = project.clone();
    let prepare_options = options.clone();
    let prepare_env = env.clone();
    let prepare_recorders = recorders.clone();
    let prepare_interactions = interactions.clone();
    let storage_project = project.clone();
    let storage_options = options.clone();
    let factory = Arc::new(ProjectSessionFactory::new(FactoryOptions {
        writer_busy_wait_timeout: options.drain_budget,
        prepare: Arc::new(move |context, _| {
            let project = prepare_project.clone();
            let options = prepare_options.clone();
            let env = prepare_env.clone();
            let recorders = prepare_recorders.clone();
            let interactions = prepare_interactions.clone();
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
                config.insert("permissionMode".into(), "default".into());
                let mut retained = PreparedResources::default();
                retained.retain_lease(lease);
                let tail = TailConfig {
                    cwd: Some(options.project.clone()),
                    auto_generate_title: true,
                    title_provider: Some(title),
                    ..Default::default()
                };
                let session = context.session_key;
                Ok(PreparedSession {
                    resources: retained,
                    tail,
                    host: Box::new(move |binding| {
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
                        )?;
                        Ok(TurnHostFactory::new_with_signal(
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
                        .with_agent_config(config))
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
        diagnostic: Arc::new(|d| eprintln!("native session: {d:?}")),
    }));
    let built = (|| -> Result<_, ProxyError> {
        let runs = Arc::new(FileRegistry::new(FileRunRegistry::open(
            options.home.join("runs.json"),
        )?)?);
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
        Ok((runs, projector))
    })();
    let (runs, projector) = match built {
        Ok(value) => value,
        Err(error) => {
            let _ = tokio::time::timeout(options.drain_budget, project.dispose()).await;
            return Err(error);
        }
    };
    let cleanup_factory = factory.clone();
    let cleanup_project = project.clone();
    let build_factory = factory.clone();
    let build_project = project.clone();
    let budget = options.drain_budget;
    let host = TurnGateway::start_with_cleanup(
        TurnReplayOptions::new(),
        move |coordinator| {
            let owner = SessionOwner::new(SessionOwnerOptions::new(build_factory));
            let replacement = Arc::new(ReplacementCoordinator::new(
                Default::default(),
                Arc::new(owner.clone()),
                budget.as_millis() as u64,
                None,
            ));
            let admission = SubmitAdmissionOptions {
                interactions: interactions.clone(),
                ..SubmitAdmissionOptions::new(replacement.clone())
            };
            let services = Arc::new(super::services::Services {
                options: options.clone(),
                project: build_project.clone(),
                env: env.clone(),
                recorders,
                owner: owner.clone(),
            });
            let requests = Arc::new(super::requests::Requests {
                project: build_project,
                options,
                env,
                coordinator: coordinator.clone(),
                runs: runs.clone(),
                owner: owner.clone(),
            });
            Ok(GatewayServices {
                interactions,
                requests,
                submitter: Arc::new(TurnSubmitter {
                    owner,
                    coordinator,
                    registry: Some(runs),
                    workspace_admissions: WorkspaceAdmissions::default(),
                    host: Arc::new(GatewaySubmitHost::new(
                        services,
                        Arc::new(NativeSubmitReplacement((*replacement).clone())),
                        Arc::new(GatewaySubmitAdmission::new(admission)),
                    )),
                    projector,
                }),
            })
        },
        async move {
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
            sessions.and(project)
        },
    );
    match host {
        Ok(host) => Ok(host),
        Err(error) => {
            let _ = tokio::time::timeout(budget, async {
                let _ = factory.dispose().await;
                project.dispose().await
            })
            .await;
            Err(error.into())
        }
    }
}
pub fn transcript_path(options: &Options, session: &JsString) -> std::path::PathBuf {
    crate::session_chat_dir(
        &options.home,
        &options.project,
        &options.project.to_string_lossy(),
    )
    .join(format!(
        "{}.jsonl",
        crate::storage::sanitize_session_id(session)
    ))
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
