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
        let family = self.family.as_str();
        let id = self.id;
        let fail = self
            .probe
            .fail_cleanup
            .lock()
            .unwrap()
            .contains(&self.family);
        Box::pin(async move {
            probe.event(format!("dispose:{family}:{id}"));
            if fail {
                Err(RuntimeError::new(format!("dispose failed:{family}:{id}")))
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
    format!("agent={}", agent_model(context))
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
    let factory = ProjectResourcesFactory::new(ProjectResourcesOptions {
        pilot_home: scratch.join("home"),
        cwd: scratch.to_path_buf(),
        env,
        homedir: Some(scratch.join("home")),
        builtin_skills_root: None,
        now: Arc::new(|| SystemTime::UNIX_EPOCH),
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
    assert_eq!(cases.len(), 9);
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
    assert_eq!(replayed, 50, "all recorded steps must be replayed");
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
