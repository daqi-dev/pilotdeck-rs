//! Replay of the real TypeScript `buildRuntime`/`ProjectRuntimeResourcesBundle`
//! oracle (tests/fixtures/project-runtime-factory) over the production
//! [`ProjectResourcesFactory`]. Only the twelve family constructors are
//! recording fakes; the config read, profile resolution, staging order,
//! rollback, and registry generations are the production paths.
use super::*;
use crate::runtime::project::{ProjectRuntime, ProjectRuntimeRegistry, RuntimeLease};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    future::Future,
    path::Path,
    sync::{
        Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};

struct Scratch(PathBuf);
impl Scratch {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "pdrs-project-runtime-factory-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir(&root).unwrap();
        Self(root)
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).unwrap();
    }
}

async fn until(mut condition: impl FnMut() -> bool) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while !condition() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("lifecycle operation did not progress");
}
async fn bounded<F: Future>(future: F) -> F::Output {
    tokio::time::timeout(Duration::from_secs(5), future)
        .await
        .expect("lifecycle operation hung")
}
/// The generator observes each step after a `setImmediate` turn, so every
/// fire-and-forget cleanup started during the step is part of that step's
/// observation. Yield until the single-threaded executor has run everything
/// already spawned; a generation that must stay alive (a leased old
/// generation) records nothing here.
async fn settle() {
    for _ in 0..16 {
        tokio::task::yield_now().await;
    }
}
fn error_json(error: &RuntimeError) -> Value {
    json!({"message":error.message, "causes":error.causes.iter().map(error_json).collect::<Vec<_>>()})
}

/// The two phases a fake family can fail in, matching the generator's
/// `fail: { family, phase }` injection points.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Phase {
    Construct,
    Stage,
}

/// Shared recording state for one oracle case: the global handle counter the
/// generator's `nextId`, the event log, and the per-step failure injections.
#[derive(Default)]
struct Probe {
    events: Mutex<Vec<String>>,
    next: AtomicUsize,
    fail: Mutex<Option<(Family, Phase)>>,
    fail_cleanup: Mutex<Vec<Family>>,
    /// Per-step cross-family disposal dependency (the generator's
    /// `disposeWaits`): the waiter family's disposal only starts once the
    /// signal family's disposal has.
    dispose_waits: Mutex<Option<(Family, Family)>>,
    started: Mutex<Vec<Family>>,
}
impl Probe {
    fn event(&self, event: String) {
        self.events.lock().unwrap().push(event);
    }
    fn events(&self) -> Vec<String> {
        self.events.lock().unwrap().clone()
    }
    fn fails(&self, family: Family, phase: Phase) -> bool {
        *self.fail.lock().unwrap() == Some((family, phase))
    }
    fn dispose_wait(&self, family: Family) -> Option<Family> {
        self.dispose_waits
            .lock()
            .unwrap()
            .filter(|(waiter, _)| *waiter == family)
            .map(|(_, signal)| signal)
    }
    fn dispose_started(&self, family: Family) -> bool {
        self.started.lock().unwrap().contains(&family)
    }
    fn mark_dispose_started(&self, family: Family) {
        self.started.lock().unwrap().push(family);
    }
    fn construct(&self, family: Family, extra: &str) -> Result<usize, RuntimeError> {
        let id = self.next.fetch_add(1, Ordering::SeqCst) + 1;
        let suffix = if extra.is_empty() {
            String::new()
        } else {
            format!(":{extra}")
        };
        self.event(format!("construct:{}:{id}{suffix}", family.as_str()));
        if self.fails(family, Phase::Construct) {
            return Err(RuntimeError::new(format!(
                "construct failed:{}:{id}",
                family.as_str()
            )));
        }
        Ok(id)
    }
    fn stage(&self, family: Family, id: usize) -> Result<(), RuntimeError> {
        self.event(format!("stage:{}:{id}", family.as_str()));
        if self.fails(family, Phase::Stage) {
            return Err(RuntimeError::new(format!(
                "stage failed:{}:{id}",
                family.as_str()
            )));
        }
        Ok(())
    }
}

/// One fake family resource: records its disposal and can fail it, like the
/// generator's per-family dispose owners.
struct FakeHandle {
    probe: Arc<Probe>,
    family: Family,
    id: usize,
}
impl FakeHandle {
    fn new(probe: &Arc<Probe>, family: Family, id: usize) -> Arc<Self> {
        Arc::new(Self {
            probe: Arc::clone(probe),
            family,
            id,
        })
    }
}
impl FamilyResource for FakeHandle {
    fn dispose(&self) -> DisposeFuture<'_> {
        let probe = Arc::clone(&self.probe);
        let family = self.family;
        let name = self.family.as_str();
        let id = self.id;
        let fail = self
            .probe
            .fail_cleanup
            .lock()
            .unwrap()
            .contains(&self.family);
        let wait = probe.dispose_wait(family);
        Box::pin(async move {
            if let Some(signal) = wait {
                // The generator's `setImmediate` poll: yield until the signal
                // family's disposal has started. Only the concurrent
                // parallel-group start can satisfy this; a sequential
                // implementation hangs and fails the step by deadline.
                while !probe.dispose_started(signal) {
                    tokio::task::yield_now().await;
                }
            }
            probe.mark_dispose_started(family);
            probe.event(format!("dispose:{name}:{id}"));
            if fail {
                Err(RuntimeError::new(format!("dispose failed:{name}:{id}")))
            } else {
                Ok(())
            }
        })
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

/// The generator's recorded handle id of an earlier family's owner/resource.
fn handle_id(handle: Option<Arc<dyn FamilyResource>>) -> String {
    handle
        .expect("earlier family must have staged its handle")
        .as_any()
        .downcast_ref::<FakeHandle>()
        .expect("fake family handle")
        .id
        .to_string()
}

// The construct-time details the generator's per-family fakes record, all
// read through the production StageContext exactly as the real families
// would read the bundle's stage inputs.
fn agent_model(context: &StageContext) -> String {
    context
        .snapshot()
        .get("config")
        .and_then(|config| config.get("agent"))
        .and_then(|agent| agent.get("model"))
        .and_then(|model| model.get("id"))
        .and_then(JsValue::as_string)
        .and_then(|id| id.to_utf8().ok())
        .unwrap_or_default()
}
fn model_runtime(context: &StageContext) -> String {
    let snapshot = context.snapshot();
    let recent = snapshot
        .get("loadedAt")
        .and_then(JsValue::as_string)
        .map(loaded_at_recent)
        .unwrap_or(false);
    format!("agent={}:loadedAtRecent={recent}", agent_model(context))
}
/// The generator's `loadedAtRecent` flag: `loadPilotConfig`'s own wall clock
/// (`new Date()`, loadPilotConfig.ts:65) stamps the snapshot even though the
/// injected resource clock is stateful — the bundle never passes
/// `options.now` to the config read (ProjectRuntimeResourcesBundle.ts:201-204).
fn loaded_at_recent(loaded_at: &pilotdeck_protocol::JsString) -> bool {
    let Ok(Some(epoch_ms)) = pilotdeck_transport::system_clock::parse_node22_date(loaded_at) else {
        return false;
    };
    let now_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as f64)
        .unwrap_or_default();
    (now_ms - epoch_ms).abs() < 3_600_000.0
}
fn builtin_plugins_enabled(context: &StageContext) -> String {
    let snapshot = context.snapshot();
    let shown = match snapshot
        .get("config")
        .and_then(|config| config.get("extension"))
        .and_then(|extension| extension.get("builtinPluginsEnabled"))
        .and_then(JsValue::as_object)
    {
        Some(enabled) => enabled.len().to_string(),
        None => "undefined".to_owned(),
    };
    format!("builtinPluginsEnabled={shown}")
}
fn router_runtime(context: &StageContext) -> String {
    format!(
        "model={}:extensions={}:registry={}:useModelRuntime=false",
        handle_id(context.resource(Resource::Model)),
        handle_id(context.owner(Family::PluginRuntime)),
        handle_id(context.owner(Family::RouterCustomRouters))
    )
}
fn execution_world(context: &StageContext) -> String {
    let profile = context.profile();
    format!(
        "profile={}/{}/{}:lsp={}",
        profile.sandbox_mode.as_str(),
        profile.interaction.name.as_str(),
        profile.runtime_context_surface.as_str(),
        handle_id(context.owner(Family::LspService))
    )
}
fn compaction(context: &StageContext) -> String {
    let millis = context
        .now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis())
        .unwrap_or_default();
    format!("now={millis}")
}
fn session_title(context: &StageContext) -> String {
    format!("model={}", handle_id(context.resource(Resource::Model)))
}
fn memory_bundle(context: &StageContext) -> String {
    format!("agentModel={}", agent_model(context))
}
fn no_extra(_: &StageContext) -> String {
    String::new()
}

/// One family's fake source: construct (recording the generator's extra),
/// register the rollback owner where the baseline assigns the field, and run
/// the stage event/published resources only for the families the baseline
/// stages.
struct FakeSource {
    probe: Arc<Probe>,
    family: Family,
    extra: fn(&StageContext) -> String,
    registers_owner: bool,
    stages: bool,
    publishes: &'static [Resource],
}
impl ResourceFamilySource for FakeSource {
    fn family(&self) -> Family {
        self.family
    }
    fn stage(&self, context: &StageContext, sink: &mut StageSink<'_>) -> RuntimeResult {
        let id = self.probe.construct(self.family, &(self.extra)(context))?;
        if self.registers_owner {
            sink.register_owner(self.family, FakeHandle::new(&self.probe, self.family, id));
        }
        if self.stages {
            self.probe.stage(self.family, id)?;
            for resource in self.publishes {
                sink.register_resource(*resource, FakeHandle::new(&self.probe, self.family, id));
            }
        }
        Ok(())
    }
}

fn family_from(name: &str) -> Family {
    [
        Family::ModelRuntime,
        Family::PluginRuntime,
        Family::RouterCustomRouters,
        Family::RouterRuntime,
        Family::LspService,
        Family::ExecutionWorld,
        Family::ContextStorage,
        Family::Compaction,
        Family::PromptCacheCoordinator,
        Family::SessionTitleProvider,
        Family::McpProvider,
        Family::MemoryBundle,
    ]
    .into_iter()
    .find(|family| family.as_str() == name)
    .unwrap_or_else(|| panic!("unknown oracle family {name}"))
}

fn families(probe: &Arc<Probe>) -> Families {
    fn source(
        probe: &Arc<Probe>,
        family: Family,
        extra: fn(&StageContext) -> String,
        registers_owner: bool,
        stages: bool,
        publishes: &'static [Resource],
    ) -> Arc<dyn ResourceFamilySource> {
        Arc::new(FakeSource {
            probe: Arc::clone(probe),
            family,
            extra,
            registers_owner,
            stages,
            publishes,
        })
    }
    // Every family the generator injects a factory for is staged here; the
    // absent ones (native defaults) never come up in the oracle.
    Families {
        model_runtime: source(
            probe,
            Family::ModelRuntime,
            model_runtime,
            true,
            true,
            &[Resource::Model, Resource::ModelProviders],
        ),
        plugins: source(
            probe,
            Family::PluginRuntime,
            builtin_plugins_enabled,
            true,
            false,
            &[],
        ),
        router_custom_routers: Some(source(
            probe,
            Family::RouterCustomRouters,
            no_extra,
            true,
            false,
            &[],
        )),
        router_runtime: source(
            probe,
            Family::RouterRuntime,
            router_runtime,
            true,
            true,
            &[Resource::TokenAccounting, Resource::Router],
        ),
        lsp: Some(source(
            probe,
            Family::LspService,
            no_extra,
            true,
            false,
            &[],
        )),
        execution_world: source(
            probe,
            Family::ExecutionWorld,
            execution_world,
            true,
            true,
            &[Resource::ExecutionWorld, Resource::Tools],
        ),
        context_storage: source(
            probe,
            Family::ContextStorage,
            no_extra,
            false,
            true,
            &[Resource::InstructionStorage, Resource::ToolResultSpill],
        ),
        compaction: Some(source(
            probe,
            Family::Compaction,
            compaction,
            true,
            false,
            &[],
        )),
        prompt_cache_coordinator: Some(source(
            probe,
            Family::PromptCacheCoordinator,
            no_extra,
            false,
            false,
            &[],
        )),
        session_title: Some(source(
            probe,
            Family::SessionTitleProvider,
            session_title,
            true,
            false,
            &[],
        )),
        mcp: source(probe, Family::McpProvider, no_extra, true, false, &[]),
        memory: source(probe, Family::MemoryBundle, memory_bundle, true, true, &[]),
    }
}

fn make_registry(scratch: &Path, probe: &Arc<Probe>) -> Arc<ProjectRuntimeRegistry> {
    let mut env = crate::config::Env::new();
    env.insert(
        "PILOT_HOME".to_owned(),
        scratch.join("config-home").to_string_lossy().into_owned(),
    );
    // The generator's stateful clock (`new Date(1000 * ++nowCalls)`): the
    // recorded compaction ticks prove how many times the resource clock was
    // consumed before each compaction construct. A config read that also
    // drew from this clock would shift every later tick.
    let ticks = Arc::new(AtomicUsize::new(0));
    let now = {
        let ticks = Arc::clone(&ticks);
        Arc::new(move || {
            UNIX_EPOCH + Duration::from_secs(ticks.fetch_add(1, Ordering::SeqCst) as u64 + 1)
        }) as Now
    };
    let factory = ProjectResourcesFactory::new(ProjectResourcesOptions {
        pilot_home: scratch.join("home"),
        cwd: scratch.to_path_buf(),
        env,
        homedir: Some(scratch.join("home")),
        builtin_skills_root: None,
        now,
        config_version: None,
        runtime_profile_overrides: RuntimeProfileOverrides::default(),
        families: families(probe),
    });
    Arc::new(ProjectRuntimeRegistry::new(
        scratch.join("fallback"),
        Arc::new(factory),
    ))
}

const GLOBAL_YAML: &str = "schemaVersion: 1\nagent: {model: parity/deterministic}\nmodel:\n  providers:\n    parity:\n      protocol: openai\n      url: http://127.0.0.1:28794/v1\n      apiKey: mock-only\n      models:\n        deterministic: {}\n        alpha: {}\n        beta: {}\n";
const PROJECT_CONFIGS: [(&str, &str); 3] = [
    ("v1", "agent: {model: parity/alpha}\n"),
    (
        "v2",
        "agent: {model: parity/beta, sandboxMode: read-only, interactionProfile: headless}\n",
    ),
    // agent.subagents must be an object: a direct PilotConfigError throw.
    ("broken", "agent: {subagents: 3}\n"),
];

/// The runtime's published model resource, i.e. the generator's
/// `runtime.model.fixtureId`.
fn model_id(runtime: &ProjectRuntime) -> usize {
    let group = runtime
        .resources()
        .get::<ProjectResources>()
        .expect("the factory registers its group");
    group
        .context()
        .resource(Resource::Model)
        .expect("model resource")
        .as_any()
        .downcast_ref::<FakeHandle>()
        .expect("fake family handle")
        .id
}

#[tokio::test]
async fn real_ts_factory_oracle() {
    let text = include_str!("../../../tests/fixtures/project-runtime-factory/oracle.json");
    crate::test_baseline::oracle(text);
    let oracle: Value = serde_json::from_str(text).unwrap();
    let cases = oracle["cases"].as_array().unwrap();
    assert_eq!(cases.len(), 12);
    let mut replayed = 0;
    for case in cases {
        let scratch = Scratch::new();
        std::fs::create_dir_all(scratch.0.join("config-home")).unwrap();
        std::fs::write(scratch.0.join("config-home/pilotdeck.yaml"), GLOBAL_YAML).unwrap();
        let probe = Arc::new(Probe::default());
        let registry = make_registry(&scratch.0, &probe);
        let mut runtimes: HashMap<String, Arc<ProjectRuntime>> = HashMap::new();
        let mut leases: HashMap<String, RuntimeLease> = HashMap::new();
        for step in case["steps"].as_array().unwrap() {
            let input = &step["input"];
            *probe.fail.lock().unwrap() = input["fail"].as_object().map(|fail| {
                (
                    family_from(fail["family"].as_str().unwrap()),
                    match fail["phase"].as_str().unwrap() {
                        "construct" => Phase::Construct,
                        "stage" => Phase::Stage,
                        phase => panic!("unknown oracle fail phase {phase}"),
                    },
                )
            });
            *probe.fail_cleanup.lock().unwrap() = input["failCleanup"]
                .as_array()
                .map(|names| {
                    names
                        .iter()
                        .map(|name| family_from(name.as_str().unwrap()))
                        .collect()
                })
                .unwrap_or_default();
            *probe.dispose_waits.lock().unwrap() = input["disposeWaits"].as_object().map(|wait| {
                (
                    family_from(wait["waiter"].as_str().unwrap()),
                    family_from(wait["signal"].as_str().unwrap()),
                )
            });
            probe.started.lock().unwrap().clear();
            let root = input["root"].as_str().map(|key| scratch.0.join(key));
            let mut resolved_id = Value::Null;
            let outcome = match input["op"].as_str().unwrap() {
                "write" => {
                    let variant = input["variant"].as_str().unwrap();
                    let (_, yaml) = PROJECT_CONFIGS
                        .iter()
                        .find(|(name, _)| name == &variant)
                        .unwrap_or_else(|| panic!("unknown config variant {variant}"));
                    let dir = scratch
                        .0
                        .join(input["project"].as_str().unwrap())
                        .join(".pilotdeck");
                    std::fs::create_dir_all(&dir).unwrap();
                    std::fs::write(dir.join("pilotdeck.yaml"), yaml).unwrap();
                    Ok(())
                }
                "resolve" => registry.resolve(root.as_deref()).map(|runtime| {
                    resolved_id = json!(model_id(&runtime));
                    if let Some(label) = input["as"].as_str() {
                        runtimes.insert(label.to_owned(), runtime);
                    }
                }),
                "acquire" => runtimes[input["runtime"].as_str().unwrap()]
                    .acquire()
                    .map(|lease| {
                        leases.insert(input["as"].as_str().unwrap().to_owned(), lease);
                    }),
                "release" => {
                    bounded(
                        leases
                            .get_mut(input["lease"].as_str().unwrap())
                            .unwrap()
                            .release(),
                    )
                    .await
                }
                "invalidate" => {
                    registry.invalidate(root.as_deref());
                    Ok(())
                }
                "reload" => bounded(registry.reload()).await,
                "dispose" => bounded(registry.dispose()).await,
                operation => panic!("unhandled oracle operation {operation}"),
            };
            let expected_events = step["events"].as_array().unwrap().len();
            until(|| probe.events().len() >= expected_events).await;
            settle().await;
            for (label, runtime) in &runtimes {
                if step["runtimes"][label]["state"] == "disposed" {
                    let _ = bounded(runtime.wait_disposed()).await;
                }
            }
            let actual = json!({
                "input":input, "id":resolved_id, "error":outcome.as_ref().err().map(error_json), "events":probe.events(),
                "runtimes":runtimes.iter().map(|(label,runtime)| (label.clone(), json!({"id":model_id(runtime),"state":runtime.state().as_str(),"leases":runtime.lease_count()}))).collect::<serde_json::Map<String,Value>>()
            });
            assert_eq!(actual, *step, "{} step {replayed}", case["name"]);
            replayed += 1;
        }
    }
    assert_eq!(replayed, 68, "all recorded steps must be replayed");
}

/// Regenerates the oracle from the real TS registry and compares bytes,
/// matching the sibling `*_fixtures_regenerate_identically_from_ts` tests.
#[test]
#[ignore = "requires PDRS_BASELINE (PilotDeck cc7c4e6 with node_modules)"]
fn project_runtime_factory_fixtures_regenerate_identically_from_ts() {
    crate::test_baseline::regenerates_identically("project-runtime-factory");
}

/// `Families::unwired` must fail every family explicitly instead of staging
/// silently: a generation built with it never publishes and reports the
/// first family (modelRuntime, in stage order) with its package tag.
#[tokio::test]
async fn unwired_families_report_explicit_errors() {
    let scratch = Scratch::new();
    std::fs::create_dir_all(scratch.0.join("a/.pilotdeck")).unwrap();
    std::fs::write(
        scratch.0.join("a/.pilotdeck/pilotdeck.yaml"),
        PROJECT_CONFIGS[0].1,
    )
    .unwrap();
    let mut env = crate::config::Env::new();
    env.insert(
        "PILOT_HOME".to_owned(),
        scratch.0.join("config-home").to_string_lossy().into_owned(),
    );
    std::fs::create_dir_all(scratch.0.join("config-home")).unwrap();
    std::fs::write(scratch.0.join("config-home/pilotdeck.yaml"), GLOBAL_YAML).unwrap();
    let factory = ProjectResourcesFactory::new(ProjectResourcesOptions {
        pilot_home: scratch.0.join("home"),
        cwd: scratch.0.to_path_buf(),
        env,
        homedir: Some(scratch.0.join("home")),
        builtin_skills_root: None,
        now: Arc::new(|| SystemTime::UNIX_EPOCH),
        config_version: None,
        runtime_profile_overrides: RuntimeProfileOverrides::default(),
        families: Families::unwired(),
    });
    let registry = ProjectRuntimeRegistry::new(scratch.0.join("fallback"), Arc::new(factory));
    let error = match registry.resolve(Some(&scratch.0.join("a"))) {
        Err(error) => error,
        Ok(_) => panic!("unwired families must not stage a generation"),
    };
    assert_eq!(
        error.message,
        "Project runtime resource family modelRuntime is not available in the native runtime yet (A6.9-pr3b)."
    );
    // The config read itself succeeded; nothing else was constructed.
    let error = match registry.resolve(Some(&scratch.0.join("a"))) {
        Err(error) => error,
        Ok(_) => panic!("retry must fail identically"),
    };
    assert!(
        error
            .message
            .starts_with("Project runtime resource family modelRuntime")
    );
    bounded(registry.dispose()).await.unwrap();
}

/// The eight parallel-group disposals start together (`Promise.allSettled`,
/// ProjectRuntimeResourcesBundle.ts:416-425): a disposal that waits for a
/// sibling's cleanup signal is unblocked by that sibling's start. A
/// sequential implementation never starts the sibling and fails here by
/// deadline.
#[tokio::test]
async fn dispose_starts_parallel_group_concurrently() {
    let probe = Arc::new(Probe::default());
    *probe.dispose_waits.lock().unwrap() = Some((Family::PluginRuntime, Family::LspService));
    let profile = RuntimeProfile {
        sandbox_mode: SandboxMode::DangerFullAccess,
        runtime_context_surface: RuntimeContextSurface::SystemPrompt,
        interaction: InteractionProfile::resolve(InteractionProfileName::Interactive),
    };
    let group = ProjectResources::new(
        PathBuf::from("/project"),
        PathBuf::from("/home"),
        None,
        crate::config::Env::new(),
        Arc::new(|| SystemTime::UNIX_EPOCH),
        Arc::new(JsValue::Undefined),
        profile,
    );
    {
        let mut sink = group.sink();
        sink.register_owner(
            Family::PluginRuntime,
            FakeHandle::new(&probe, Family::PluginRuntime, 1),
        );
        sink.register_owner(
            Family::LspService,
            FakeHandle::new(&probe, Family::LspService, 2),
        );
        sink.register_owner(
            Family::MemoryBundle,
            FakeHandle::new(&probe, Family::MemoryBundle, 3),
        );
    }
    bounded(group.dispose()).await.unwrap();
    // lspService starts in the same pass as the rest of the group;
    // pluginRuntime's wait resolves only after it, and memoryBundle waits
    // for the whole group.
    assert_eq!(
        probe.events(),
        [
            "dispose:lspService:2",
            "dispose:pluginRuntime:1",
            "dispose:memoryBundle:3"
        ]
    );
}

/// The config snapshot's `loadedAt` is the config read's own wall clock
/// (`new Date()`, loadPilotConfig.ts:65), never the injected resource clock:
/// with a frozen epoch clock the snapshot still carries a current timestamp,
/// and the resource clock is not consumed by the config read.
#[tokio::test]
async fn config_loaded_at_uses_wall_clock_not_resource_clock() {
    let scratch = Scratch::new();
    std::fs::create_dir_all(scratch.0.join("a/.pilotdeck")).unwrap();
    std::fs::write(
        scratch.0.join("a/.pilotdeck/pilotdeck.yaml"),
        PROJECT_CONFIGS[0].1,
    )
    .unwrap();
    std::fs::create_dir_all(scratch.0.join("config-home")).unwrap();
    std::fs::write(scratch.0.join("config-home/pilotdeck.yaml"), GLOBAL_YAML).unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let now = {
        let calls = Arc::clone(&calls);
        Arc::new(move || {
            calls.fetch_add(1, Ordering::SeqCst);
            SystemTime::UNIX_EPOCH
        }) as Now
    };
    let loaded_at: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    struct CapturingSource {
        loaded_at: Arc<Mutex<Option<String>>>,
    }
    impl ResourceFamilySource for CapturingSource {
        fn family(&self) -> Family {
            Family::ModelRuntime
        }
        fn stage(&self, context: &StageContext, _: &mut StageSink<'_>) -> RuntimeResult {
            *self.loaded_at.lock().unwrap() = context
                .snapshot()
                .get("loadedAt")
                .and_then(JsValue::as_string)
                .and_then(|value| value.to_utf8().ok());
            // Staging stops right after the config read was observed.
            Err(RuntimeError::new("stop staging here"))
        }
    }
    let mut env = crate::config::Env::new();
    env.insert(
        "PILOT_HOME".to_owned(),
        scratch.0.join("config-home").to_string_lossy().into_owned(),
    );
    let factory = ProjectResourcesFactory::new(ProjectResourcesOptions {
        pilot_home: scratch.0.join("home"),
        cwd: scratch.0.to_path_buf(),
        env,
        homedir: Some(scratch.0.join("home")),
        builtin_skills_root: None,
        now,
        config_version: None,
        runtime_profile_overrides: RuntimeProfileOverrides::default(),
        families: Families {
            model_runtime: Arc::new(CapturingSource {
                loaded_at: Arc::clone(&loaded_at),
            }),
            ..Families::unwired()
        },
    });
    let registry = ProjectRuntimeRegistry::new(scratch.0.join("fallback"), Arc::new(factory));
    let error = match registry.resolve(Some(&scratch.0.join("a"))) {
        Err(error) => error,
        Ok(_) => panic!("staging stops after the config read"),
    };
    assert_eq!(error.message, "stop staging here");
    let loaded_at = loaded_at.lock().unwrap().clone().expect("config was read");
    let loaded_at = pilotdeck_protocol::JsString::from(loaded_at);
    assert!(
        loaded_at_recent(&loaded_at),
        "loadedAt must be the config read's wall clock, not the frozen resource clock"
    );
    assert_eq!(
        calls.load(Ordering::SeqCst),
        0,
        "the config read must not consume the injected resource clock"
    );
    bounded(registry.dispose()).await.unwrap();
}

/// Every family `Families::unwired` reports carries its own name and package
/// tag, not only the first one in stage order: the families staged before it
/// are no-ops, so each unsupported family is the one that fails the
/// generation. Compaction and the prompt-cache coordinator are absent by
/// default in both the baseline and `Families::unwired` and report nothing.
#[tokio::test]
async fn unwired_families_report_each_family() {
    const STAGE_ORDER: [Family; 12] = [
        Family::ModelRuntime,
        Family::PluginRuntime,
        Family::RouterCustomRouters,
        Family::RouterRuntime,
        Family::LspService,
        Family::ExecutionWorld,
        Family::ContextStorage,
        Family::Compaction,
        Family::PromptCacheCoordinator,
        Family::SessionTitleProvider,
        Family::McpProvider,
        Family::MemoryBundle,
    ];
    struct NoopSource(Family);
    impl ResourceFamilySource for NoopSource {
        fn family(&self) -> Family {
            self.0
        }
        fn stage(&self, _: &StageContext, _: &mut StageSink<'_>) -> RuntimeResult {
            Ok(())
        }
    }
    fn noop(families: &mut Families, family: Family) {
        match family {
            Family::ModelRuntime => families.model_runtime = Arc::new(NoopSource(family)),
            Family::PluginRuntime => families.plugins = Arc::new(NoopSource(family)),
            Family::RouterCustomRouters => {
                families.router_custom_routers = Some(Arc::new(NoopSource(family)))
            }
            Family::RouterRuntime => families.router_runtime = Arc::new(NoopSource(family)),
            Family::LspService => families.lsp = Some(Arc::new(NoopSource(family))),
            Family::ExecutionWorld => families.execution_world = Arc::new(NoopSource(family)),
            // ContextStorage and MemoryBundle are now implemented (A6.9-pr3d)
            Family::ContextStorage => {}
            Family::SessionTitleProvider => {
                families.session_title = Some(Arc::new(NoopSource(family)))
            }
            Family::McpProvider => families.mcp = Arc::new(NoopSource(family)),
            Family::MemoryBundle => {}
            // Absent by default in the baseline; `unwired` leaves them out.
            Family::Compaction | Family::PromptCacheCoordinator => {}
        }
    }
    let scratch = Scratch::new();
    std::fs::create_dir_all(scratch.0.join("a/.pilotdeck")).unwrap();
    std::fs::write(
        scratch.0.join("a/.pilotdeck/pilotdeck.yaml"),
        PROJECT_CONFIGS[0].1,
    )
    .unwrap();
    std::fs::create_dir_all(scratch.0.join("config-home")).unwrap();
    std::fs::write(scratch.0.join("config-home/pilotdeck.yaml"), GLOBAL_YAML).unwrap();
    for (family, tag) in [
        (Family::ModelRuntime, "A6.9-pr3b"),
        (Family::PluginRuntime, "A6.9-pr3e"),
        (
            Family::RouterCustomRouters,
            "A6.9-pr3e createNativeRouterSessionCustomRouterPort",
        ),
        (Family::RouterRuntime, "A6.9-pr3b"),
        (Family::LspService, "A6.9-pr3c LspService"),
        (Family::ExecutionWorld, "A6.9-pr3c"),
        // ContextStorage and MemoryBundle are now implemented (A6.9-pr3d)
        (
            Family::SessionTitleProvider,
            "A6.9-pr3d createNativeSessionTitleProvider",
        ),
        (Family::McpProvider, "A6.9-pr3c"),
    ] {
        let mut families = Families::unwired();
        for before in STAGE_ORDER
            .into_iter()
            .take_while(|&staged| staged != family)
        {
            noop(&mut families, before);
        }
        let mut env = crate::config::Env::new();
        env.insert(
            "PILOT_HOME".to_owned(),
            scratch.0.join("config-home").to_string_lossy().into_owned(),
        );
        let factory = ProjectResourcesFactory::new(ProjectResourcesOptions {
            pilot_home: scratch.0.join("home"),
            cwd: scratch.0.to_path_buf(),
            env,
            homedir: Some(scratch.0.join("home")),
            builtin_skills_root: None,
            now: Arc::new(|| SystemTime::UNIX_EPOCH),
            config_version: None,
            runtime_profile_overrides: RuntimeProfileOverrides::default(),
            families,
        });
        let registry = ProjectRuntimeRegistry::new(scratch.0.join("fallback"), Arc::new(factory));
        let error = match registry.resolve(Some(&scratch.0.join("a"))) {
            Err(error) => error,
            Ok(_) => panic!("{family:?} must fail the generation"),
        };
        assert_eq!(
            error.message,
            format!(
                "Project runtime resource family {} is not available in the native runtime yet ({}).",
                family.as_str(),
                tag
            ),
            "family {family:?}"
        );
        bounded(registry.dispose()).await.unwrap();
    }
}
