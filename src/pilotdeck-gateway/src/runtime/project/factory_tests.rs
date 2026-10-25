//! Replay of the real TypeScript `buildRuntime`/`ProjectRuntimeResourcesBundle`
//! oracle (tests/fixtures/project-runtime-factory) over the production
//! [`ProjectResourcesFactory`]. Only the twelve family constructors are
//! recording fakes; the config read, profile resolution, staging order,
//! rollback, and registry generations are the production paths.
use super::*;
use crate::runtime::project::{ProjectRuntime, ProjectRuntimeRegistry, RuntimeLease};
use pilotdeck_protocol::{JsObject, JsString};
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

/// Real YAML through the production `NativeConfigSource`: a model entry with no
/// explicit capabilities and no `memory` section, the shape other tests already
/// use as `models: {mock: {}}`.
const CONFIG_YAML_NO_MEMORY: &str = "schemaVersion: 1\nagent: {model: test/mock}\nmodel:\n  providers:\n    test:\n      protocol: openai\n      url: http://127.0.0.1:28794/v1\n      apiKey: mock\n      models:\n        mock: {}\n";

/// A family that stages successfully without publishing anything; it isolates
/// one family under test inside the production registry.
struct SilentFamily(Family);
impl ResourceFamilySource for SilentFamily {
    fn family(&self) -> Family {
        self.0
    }
    fn stage(&self, _: &StageContext, _: &mut StageSink<'_>) -> RuntimeResult {
        Ok(())
    }
}
fn silent(family: Family) -> Arc<dyn ResourceFamilySource> {
    Arc::new(SilentFamily(family))
}
/// Wire every family staged before context storage/memory so the production
/// registry reaches the family under test without an unrelated unsupported
/// family failing first.
fn silent_families() -> Families {
    Families {
        model_runtime: silent(Family::ModelRuntime),
        plugins: silent(Family::PluginRuntime),
        router_custom_routers: Some(silent(Family::RouterCustomRouters)),
        router_runtime: silent(Family::RouterRuntime),
        lsp: Some(silent(Family::LspService)),
        execution_world: silent(Family::ExecutionWorld),
        session_title: Some(silent(Family::SessionTitleProvider)),
        mcp: silent(Family::McpProvider),
        ..Families::unwired()
    }
}
/// Write the global and project config the real loader reads, exactly like the
/// existing registry tests.
fn write_project_config(scratch: &Scratch, yaml: &str) -> Env {
    std::fs::create_dir_all(scratch.0.join("project/.pilotdeck")).unwrap();
    std::fs::write(scratch.0.join("project/.pilotdeck/pilotdeck.yaml"), yaml).unwrap();
    std::fs::create_dir_all(scratch.0.join("config-home")).unwrap();
    std::fs::write(scratch.0.join("config-home/pilotdeck.yaml"), yaml).unwrap();
    let mut env = Env::new();
    env.insert(
        "PILOT_HOME".into(),
        scratch.0.join("config-home").to_string_lossy().into_owned(),
    );
    env
}
/// One real `ProjectRuntimeRegistry` over `NativeConfigSource`; only the
/// staged families are fakes.
fn registry_for_yaml(scratch: &Scratch, yaml: &str, families: Families) -> ProjectRuntimeRegistry {
    let factory = ProjectResourcesFactory::new(ProjectResourcesOptions {
        pilot_home: scratch.0.join("pilot-home"),
        cwd: scratch.0.to_path_buf(),
        env: write_project_config(scratch, yaml),
        homedir: Some(scratch.0.join("pilot-home")),
        builtin_skills_root: None,
        now: Arc::new(|| SystemTime::UNIX_EPOCH),
        telemetry: None,
        config_version: None,
        runtime_profile_overrides: RuntimeProfileOverrides::default(),
        families,
    });
    ProjectRuntimeRegistry::new(scratch.0.join("project"), Arc::new(factory))
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

/// How many recorded events start with `prefix` (e.g. one family's construct,
/// stage or dispose entries).
fn count_events(events: &[String], prefix: &str) -> usize {
    events
        .iter()
        .filter(|event| event.starts_with(prefix))
        .count()
}

fn has_event(events: &[String], event: &str) -> bool {
    events.iter().any(|recorded| recorded == event)
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
        telemetry: None,
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
        telemetry: None,
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

/// A staging group with no family handles, for direct family-source tests.
fn interaction_test_group() -> ProjectResources {
    ProjectResources::new(
        PathBuf::from("/project"),
        PathBuf::from("/home"),
        None,
        crate::config::Env::new(),
        Arc::new(|| SystemTime::UNIX_EPOCH),
        Arc::new(JsValue::Undefined),
        RuntimeProfile::resolve(&JsValue::Undefined, &RuntimeProfileOverrides::default()),
    )
}

/// Stage the MCP and execution-world builders in the factory's order.
fn stage_tools_and_execution_world(
    families: &Families,
    context: &StageContext,
    sink: &mut StageSink<'_>,
) {
    families.execution_world.stage(context, sink).unwrap();
    families.mcp.stage(context, sink).unwrap();
}

fn assert_no_interaction_resources(context: &StageContext) {
    for resource in [
        Resource::Interaction,
        Resource::InteractionEvents,
        Resource::InteractionPermission,
        Resource::InteractionHooks,
        Resource::InteractionSteer,
    ] {
        assert!(
            context.resource(resource).is_none(),
            "{resource:?} must not be published",
        );
    }
}

/// Forward order: interaction layered after the tools family keeps the
/// execution-world resource/owner and the MCP family.
#[tokio::test]
async fn interaction_staging_publishes_resources() {
    let group = interaction_test_group();
    let context = group.context();
    assert_no_interaction_resources(&context);
    assert!(context.resource(Resource::ExecutionWorld).is_none());

    let families = Families::unwired().with_tools_mcp().with_interaction();
    stage_tools_and_execution_world(&families, &context, &mut group.sink());

    let interaction = context
        .resource(Resource::Interaction)
        .expect("successful interaction staging must publish its resource");
    let execution_world = context
        .resource(Resource::ExecutionWorld)
        .expect("interaction wiring must preserve the execution-world resource");
    let owner = context.owner(Family::ExecutionWorld).unwrap();
    assert!(Arc::ptr_eq(&owner, &execution_world));
    assert!(!Arc::ptr_eq(&interaction, &execution_world));
    assert!(
        context.owner(Family::McpProvider).is_some(),
        "with_tools_mcp must still wire the MCP family"
    );
    bounded(group.dispose()).await.unwrap();
}

/// Reverse order (P2a regression): `with_interaction` before `with_tools_mcp`
/// must keep MCP, the execution world and the interaction marker. Both
/// builders used to overwrite `execution_world`, silently dropping Interaction.
#[tokio::test]
async fn interaction_before_tools_keeps_all_capabilities() {
    let group = interaction_test_group();
    let context = group.context();

    let families = Families::unwired().with_interaction().with_tools_mcp();
    stage_tools_and_execution_world(&families, &context, &mut group.sink());

    let interaction = context
        .resource(Resource::Interaction)
        .expect("interaction must survive a later with_tools_mcp");
    let execution_world = context
        .resource(Resource::ExecutionWorld)
        .expect("with_tools_mcp must still publish the execution world");
    let owner = context.owner(Family::ExecutionWorld).unwrap();
    assert!(Arc::ptr_eq(&owner, &execution_world));
    assert!(!Arc::ptr_eq(&interaction, &execution_world));
    assert!(
        context.owner(Family::McpProvider).is_some(),
        "with_tools_mcp must still wire the MCP family"
    );
    bounded(group.dispose()).await.unwrap();
}

/// Negative control: MCP + execution world alone must not publish the
/// interaction marker, so an accidental always-on marker cannot pass.
#[tokio::test]
async fn tools_without_interaction_has_no_interaction_marker() {
    let group = interaction_test_group();
    let context = group.context();

    let families = Families::unwired().with_tools_mcp();
    stage_tools_and_execution_world(&families, &context, &mut group.sink());
    assert_no_interaction_resources(&context);
    assert!(context.resource(Resource::ExecutionWorld).is_some());
    assert!(context.owner(Family::McpProvider).is_some());
    bounded(group.dispose()).await.unwrap();
}

/// A custom execution-world source is wrapped, not replaced, by
/// `with_interaction`: its owner/resource identity survives.
#[tokio::test]
async fn interaction_preserves_custom_execution_world_source() {
    let probe = Arc::new(Probe::default());
    let custom: Arc<dyn ResourceFamilySource> = Arc::new(FakeSource {
        probe: Arc::clone(&probe),
        family: Family::ExecutionWorld,
        extra: no_extra,
        registers_owner: true,
        stages: true,
        publishes: &[Resource::ExecutionWorld],
    });
    let families = Families {
        execution_world: custom,
        ..Families::unwired()
    }
    .with_interaction();

    let group = interaction_test_group();
    let context = group.context();
    families
        .execution_world
        .stage(&context, &mut group.sink())
        .unwrap();

    let owner = context.owner(Family::ExecutionWorld).unwrap();
    let execution_world = context.resource(Resource::ExecutionWorld).unwrap();
    assert!(owner.as_any().is::<FakeHandle>());
    assert!(execution_world.as_any().is::<FakeHandle>());
    assert_eq!(
        handle_id(Some(owner)),
        handle_id(Some(execution_world)),
        "the custom source's owner and resource must stay the same stage"
    );
    assert!(context.resource(Resource::Interaction).is_some());
    bounded(group.dispose()).await.unwrap();
    // The wrapper must stage the custom source exactly once and forward its
    // disposal exactly once. These counts pin only a direct duplicate
    // stage/dispose (or a lost owner, which would leave no dispose event);
    // repeated wrapping itself is caught by Arc::ptr_eq, not by them.
    assert_eq!(
        probe.events(),
        [
            "construct:executionWorld:1",
            "stage:executionWorld:1",
            "dispose:executionWorld:1",
        ]
    );
}

/// Stage one interaction-wrapped execution world plus the MCP family and
/// assert the full capability set, then dispose the group. Every caller has
/// just run `with_tools_mcp`, so the staged execution world must be the native
/// source rather than a fixture kernel.
async fn assert_staged_interaction(families: &Families) {
    assert!(families.execution_world.layers_interaction());
    let group = interaction_test_group();
    let context = group.context();
    stage_tools_and_execution_world(families, &context, &mut group.sink());
    let interaction = context
        .resource(Resource::Interaction)
        .expect("the chain must publish the interaction marker");
    let execution_world = context
        .resource(Resource::ExecutionWorld)
        .expect("the chain must publish the execution world");
    assert!(
        !execution_world.as_any().is::<FakeHandle>(),
        "with_tools_mcp must stage the native execution world, not a custom kernel"
    );
    let owner = context
        .owner(Family::ExecutionWorld)
        .expect("the chain must register the execution-world owner");
    assert!(Arc::ptr_eq(&owner, &execution_world));
    assert!(!Arc::ptr_eq(&interaction, &execution_world));
    assert!(context.owner(Family::McpProvider).is_some());
    bounded(group.dispose()).await.unwrap();
}

/// `with_interaction` is idempotent: a second call must keep the same wrapper
/// Arc instead of wrapping again. Arc::ptr_eq is the direct guard; the exact
/// event sequence only pins that this wrapper stages and disposes the inner
/// source once, i.e. it has no direct duplicate stage/dispose.
#[tokio::test]
async fn repeated_interaction_builder_does_not_double_wrap() {
    let probe = Arc::new(Probe::default());
    let custom: Arc<dyn ResourceFamilySource> = Arc::new(FakeSource {
        probe: Arc::clone(&probe),
        family: Family::ExecutionWorld,
        extra: no_extra,
        registers_owner: true,
        stages: true,
        publishes: &[Resource::ExecutionWorld],
    });
    let once = Families {
        execution_world: custom,
        ..Families::unwired()
    }
    .with_interaction();
    let wrapper = Arc::clone(&once.execution_world);
    let twice = once.with_interaction();

    assert!(
        Arc::ptr_eq(&wrapper, &twice.execution_world),
        "a repeated with_interaction must reuse the existing wrapper"
    );
    let group = interaction_test_group();
    let context = group.context();
    twice
        .execution_world
        .stage(&context, &mut group.sink())
        .unwrap();
    assert!(context.resource(Resource::Interaction).is_some());
    bounded(group.dispose()).await.unwrap();
    assert_eq!(
        probe.events(),
        [
            "construct:executionWorld:1",
            "stage:executionWorld:1",
            "dispose:executionWorld:1",
        ],
        "the inner source must be staged and disposed exactly once"
    );
}

/// A supported transparent decorator forwards `layers_interaction` from the
/// source it delegates to, so the builders recognize the existing layering and
/// do not wrap it again. A decorator that falls back to the default `false`
/// is outside the contract and is treated as an ordinary source. A later
/// `with_tools_mcp` still keeps the marker but rebuilds it around the native
/// source, replacing the decorator and its custom kernel.
#[tokio::test]
async fn forwarding_decorator_keeps_interaction_layering() {
    struct ForwardingDecorator(Arc<dyn ResourceFamilySource>);
    impl ResourceFamilySource for ForwardingDecorator {
        fn family(&self) -> Family {
            self.0.family()
        }
        fn stage(&self, context: &StageContext, sink: &mut StageSink<'_>) -> RuntimeResult {
            self.0.stage(context, sink)
        }
        fn layers_interaction(&self) -> bool {
            self.0.layers_interaction()
        }
    }

    let probe = Arc::new(Probe::default());
    let custom: Arc<dyn ResourceFamilySource> = Arc::new(FakeSource {
        probe: Arc::clone(&probe),
        family: Family::ExecutionWorld,
        extra: no_extra,
        registers_owner: true,
        stages: true,
        publishes: &[Resource::ExecutionWorld],
    });
    let wrapped: Arc<dyn ResourceFamilySource> =
        Arc::new(crate::runtime::project::interaction::InteractionFamilySource::new(custom));
    let decorated: Arc<dyn ResourceFamilySource> = Arc::new(ForwardingDecorator(wrapped));
    let families = Families {
        execution_world: decorated,
        ..Families::unwired()
    };
    let decorator = Arc::clone(&families.execution_world);
    let families = families.with_interaction();

    assert!(
        Arc::ptr_eq(&decorator, &families.execution_world),
        "a forwarding decorator must be recognized as already interaction-layered"
    );
    let group = interaction_test_group();
    let context = group.context();
    families
        .execution_world
        .stage(&context, &mut group.sink())
        .unwrap();
    assert!(context.resource(Resource::Interaction).is_some());
    assert!(context.resource(Resource::ExecutionWorld).is_some());
    bounded(group.dispose()).await.unwrap();
    assert_eq!(
        probe.events(),
        [
            "construct:executionWorld:1",
            "stage:executionWorld:1",
            "dispose:executionWorld:1",
        ]
    );

    // Second segment: even after a forwarding decorator, `with_tools_mcp`
    // keeps the marker but rebuilds the layering as a fresh
    // InteractionFamilySource around the native source. The decorator and the
    // custom kernel it wrapped are both replaced, so the discarded custom
    // source must never be constructed, staged or disposed.
    let events_before = probe.events();
    let families = families.with_tools_mcp();
    assert!(
        !Arc::ptr_eq(&decorator, &families.execution_world),
        "with_tools_mcp must replace the decorated custom chain"
    );
    assert_staged_interaction(&families).await;
    assert_eq!(
        probe.events(),
        events_before,
        "the discarded decorator and custom kernel must not be staged or disposed"
    );
}

/// Paired with forwarding_decorator_keeps_interaction_layering: the default
/// false declaration deliberately hides an inner interaction selection.
#[tokio::test]
async fn nonforwarding_decorator_is_replaced_or_rewrapped() {
    struct NonForwarding(Arc<dyn ResourceFamilySource>);
    impl ResourceFamilySource for NonForwarding {
        fn family(&self) -> Family {
            self.0.family()
        }
        fn stage(&self, context: &StageContext, sink: &mut StageSink<'_>) -> RuntimeResult {
            self.0.stage(context, sink)
        }
    }
    let probe = Arc::new(Probe::default());
    let custom: Arc<dyn ResourceFamilySource> = Arc::new(FakeSource {
        probe: probe.clone(),
        family: Family::ExecutionWorld,
        extra: no_extra,
        registers_owner: true,
        stages: true,
        publishes: &[Resource::ExecutionWorld],
    });
    let decorated: Arc<dyn ResourceFamilySource> = Arc::new(NonForwarding(Arc::new(
        crate::runtime::project::interaction::InteractionFamilySource::new(custom),
    )));
    assert!(!decorated.layers_interaction());
    let replaced = Families {
        execution_world: decorated.clone(),
        ..Families::unwired()
    }
    .with_tools_mcp();
    assert!(!Arc::ptr_eq(&decorated, &replaced.execution_world));
    assert!(!replaced.execution_world.layers_interaction());
    let group = interaction_test_group();
    let context = group.context();
    replaced
        .execution_world
        .stage(&context, &mut group.sink())
        .unwrap();
    assert_no_interaction_resources(&context);
    assert!(
        !context
            .resource(Resource::ExecutionWorld)
            .unwrap()
            .as_any()
            .is::<FakeHandle>()
    );
    bounded(group.dispose()).await.unwrap();
    assert!(
        probe.events().is_empty(),
        "replaced custom must never stage"
    );

    let wrapped = Families {
        execution_world: decorated.clone(),
        ..Families::unwired()
    }
    .with_interaction();
    assert!(
        !Arc::ptr_eq(&decorated, &wrapped.execution_world),
        "false must force a new layer"
    );
    assert!(wrapped.execution_world.layers_interaction());
    let outer = wrapped.execution_world.clone();
    let wrapped = wrapped.with_interaction();
    assert!(
        Arc::ptr_eq(&outer, &wrapped.execution_world),
        "declared outer layer must not stack again"
    );
    let group = interaction_test_group();
    let context = group.context();
    assert_no_interaction_resources(&context);
    wrapped
        .execution_world
        .stage(&context, &mut group.sink())
        .unwrap();
    assert!(context.resource(Resource::Interaction).is_some());
    let owner = context.owner(Family::ExecutionWorld).unwrap();
    assert!(owner.as_any().is::<FakeHandle>());
    assert_eq!(
        handle_id(Some(owner)),
        handle_id(context.resource(Resource::ExecutionWorld)),
        "owner and resource must come from the same custom stage"
    );
    bounded(group.dispose()).await.unwrap();
    assert_eq!(
        probe.events(),
        [
            "construct:executionWorld:1",
            "stage:executionWorld:1",
            "dispose:executionWorld:1"
        ]
    );
}

/// Builder chains around the interaction selection keep a working marker
/// layering over the native source: I→T→I, T→I→T and a repeated T all stage
/// the native execution world with the interaction marker. The captured
/// wrapper Arcs pin identity only: `with_interaction` over an already-layered
/// source returns the same outer Arc, while `with_tools_mcp` returns a
/// different one. A changed outer Arc cannot by itself tell "rebuilt around
/// the native kernel" apart from "wrapped the previous wrapper": the inner
/// source is private and both shapes pass `assert_staged_interaction`. These
/// two native chains therefore do not prove non-stacking; that a custom
/// source is replaced is shown separately by
/// `tools_mcp_replaces_custom_execution_world_after_interaction`.
#[tokio::test]
async fn repeated_builder_chains_keep_interaction_capability() {
    // I -> T -> I: with_interaction keeps the outer Arc's identity (no new
    // outer layer); this is an identity pin, not a stacking oracle.
    let chained = Families::unwired().with_interaction().with_tools_mcp();
    let wrapper = Arc::clone(&chained.execution_world);
    let chained = chained.with_interaction();
    assert!(
        Arc::ptr_eq(&wrapper, &chained.execution_world),
        "with_interaction after with_tools_mcp must not double-wrap"
    );
    assert_staged_interaction(&chained).await;

    // T -> I -> T. The Arc check below is identity-only: a fresh outer Arc
    // may be a rebuild or a wrap of the previous wrapper.
    let chained = Families::unwired().with_tools_mcp().with_interaction();
    let first_wrapper = Arc::clone(&chained.execution_world);
    let chained = chained.with_tools_mcp();
    assert!(
        !Arc::ptr_eq(&first_wrapper, &chained.execution_world),
        "with_tools_mcp must return a new outer execution-world Arc; \
         identity alone cannot distinguish a rebuild from wrapping the previous wrapper"
    );
    assert_staged_interaction(&chained).await;

    // I -> T -> T: a repeated with_tools_mcp also returns a new outer Arc.
    // It neither proves nor rules out stacking, since the inner source is
    // private; the assertion pins identity change only.
    let chained = Families::unwired().with_interaction().with_tools_mcp();
    let first_wrapper = Arc::clone(&chained.execution_world);
    let chained = chained.with_tools_mcp();
    assert!(
        !Arc::ptr_eq(&first_wrapper, &chained.execution_world),
        "a repeated with_tools_mcp must return a new outer execution-world Arc; \
         identity alone cannot distinguish a rebuild from wrapping the previous wrapper"
    );
    assert_staged_interaction(&chained).await;
}

/// `with_tools_mcp` replaces a custom execution-world source by design, even
/// when `with_interaction` was layered first: only the marker selection
/// survives, the discarded custom kernel never stages.
#[tokio::test]
async fn tools_mcp_replaces_custom_execution_world_after_interaction() {
    let probe = Arc::new(Probe::default());
    let custom: Arc<dyn ResourceFamilySource> = Arc::new(FakeSource {
        probe: Arc::clone(&probe),
        family: Family::ExecutionWorld,
        extra: no_extra,
        registers_owner: true,
        stages: true,
        publishes: &[Resource::ExecutionWorld],
    });
    let families = Families {
        execution_world: custom,
        ..Families::unwired()
    }
    .with_interaction()
    .with_tools_mcp();

    assert!(families.execution_world.layers_interaction());
    assert_eq!(families.execution_world.family(), Family::ExecutionWorld);
    let group = interaction_test_group();
    let context = group.context();
    families
        .execution_world
        .stage(&context, &mut group.sink())
        .unwrap();

    assert!(context.resource(Resource::Interaction).is_some());
    let execution_world = context
        .resource(Resource::ExecutionWorld)
        .expect("with_tools_mcp must publish the native execution world");
    assert!(
        !execution_world.as_any().is::<FakeHandle>(),
        "the custom execution-world source must be replaced by the native one"
    );
    let owner = context.owner(Family::ExecutionWorld).unwrap();
    assert!(Arc::ptr_eq(&owner, &execution_world));
    assert!(
        probe.events().is_empty(),
        "a replaced custom source must not be constructed or staged"
    );
    bounded(group.dispose()).await.unwrap();
    assert!(
        probe.events().is_empty(),
        "a replaced custom source must not be disposed"
    );
}

/// The marker records only the builder selection: publishing it must leave the
/// frozen runtime profile and its permission policy untouched for every
/// profile, including Disabled and Headless.
#[tokio::test]
async fn interaction_marker_does_not_change_runtime_profile() {
    for name in [
        InteractionProfileName::Interactive,
        InteractionProfileName::Headless,
        InteractionProfileName::Disabled,
    ] {
        let profile = RuntimeProfile::resolve(
            &JsValue::Undefined,
            &RuntimeProfileOverrides {
                interaction_profile_override: Some(name),
                ..RuntimeProfileOverrides::default()
            },
        );
        let group = ProjectResources::new(
            PathBuf::from("/project"),
            PathBuf::from("/home"),
            None,
            crate::config::Env::new(),
            Arc::new(|| SystemTime::UNIX_EPOCH),
            Arc::new(JsValue::Undefined),
            profile,
        );
        let context = group.context();
        assert_eq!(context.profile(), profile);

        let probe = Arc::new(Probe::default());
        let custom: Arc<dyn ResourceFamilySource> = Arc::new(FakeSource {
            probe,
            family: Family::ExecutionWorld,
            extra: no_extra,
            registers_owner: true,
            stages: true,
            publishes: &[Resource::ExecutionWorld],
        });
        let families = Families {
            execution_world: custom,
            ..Families::unwired()
        }
        .with_interaction();
        families
            .execution_world
            .stage(&context, &mut group.sink())
            .unwrap();

        assert!(
            context.resource(Resource::Interaction).is_some(),
            "the marker is a builder selection independent of the profile"
        );
        let observed = context.profile();
        assert_eq!(observed, profile, "the marker must not change the profile");
        assert_eq!(observed.interaction.name, name);
        assert_eq!(
            observed.interaction.can_prompt,
            profile.interaction.can_prompt
        );
        assert_eq!(
            observed.interaction.permission_provider,
            profile.interaction.permission_provider
        );
        bounded(group.dispose()).await.unwrap();
    }
}

/// Negative control: a wrapped execution-world source that fails in the stage
/// phase must abort the whole wrapper. The inner error is forwarded unchanged
/// (no extra cause, no replacement message) and the interaction marker is not
/// published: staging a capability the execution world did not produce must
/// fail closed. The inner source's rollback owner, registered before its stage
/// threw, still exists and is disposed by the group.
#[tokio::test]
async fn interaction_staging_failure_propagates_without_marker() {
    let probe = Arc::new(Probe::default());
    *probe.fail.lock().unwrap() = Some((Family::ExecutionWorld, Phase::Stage));
    let custom: Arc<dyn ResourceFamilySource> = Arc::new(FakeSource {
        probe: Arc::clone(&probe),
        family: Family::ExecutionWorld,
        extra: no_extra,
        registers_owner: true,
        stages: true,
        publishes: &[Resource::ExecutionWorld],
    });
    let families = Families {
        execution_world: custom,
        ..Families::unwired()
    }
    .with_interaction();

    let group = interaction_test_group();
    let context = group.context();
    let error = families
        .execution_world
        .stage(&context, &mut group.sink())
        .expect_err("a failing execution-world stage must abort interaction staging");

    assert_eq!(
        error,
        RuntimeError::new("stage failed:executionWorld:1"),
        "the wrapper must forward the inner stage error unchanged"
    );
    assert_no_interaction_resources(&context);
    assert!(context.resource(Resource::ExecutionWorld).is_none());
    // The inner source registered its dispose owner before its stage threw; the
    // wrapper must neither replace that owner nor leak it past disposal.
    assert_eq!(handle_id(context.owner(Family::ExecutionWorld)), "1");
    bounded(group.dispose()).await.unwrap();
    assert_eq!(
        probe.events(),
        [
            "construct:executionWorld:1",
            "stage:executionWorld:1",
            "dispose:executionWorld:1"
        ]
    );
    assert_no_interaction_resources(&context);
}

/// Negative control paired with the stage-phase failure above: an inner
/// execution-world source that fails in the construct phase (before it can
/// register a rollback owner) must forward the original error, publish neither
/// the marker nor an execution-world resource, and leave nothing for disposal.
#[tokio::test]
async fn interaction_construct_failure_has_no_owner_or_marker() {
    let probe = Arc::new(Probe::default());
    *probe.fail.lock().unwrap() = Some((Family::ExecutionWorld, Phase::Construct));
    let custom: Arc<dyn ResourceFamilySource> = Arc::new(FakeSource {
        probe: Arc::clone(&probe),
        family: Family::ExecutionWorld,
        extra: no_extra,
        registers_owner: true,
        stages: true,
        publishes: &[Resource::ExecutionWorld],
    });
    let families = Families {
        execution_world: custom,
        ..Families::unwired()
    }
    .with_interaction();

    let group = interaction_test_group();
    let context = group.context();
    let error = families
        .execution_world
        .stage(&context, &mut group.sink())
        .expect_err("a failing execution-world construct must abort interaction staging");

    assert_eq!(
        error,
        RuntimeError::new("construct failed:executionWorld:1"),
        "the wrapper must forward the inner construct error unchanged"
    );
    assert!(context.owner(Family::ExecutionWorld).is_none());
    assert!(context.resource(Resource::ExecutionWorld).is_none());
    assert_no_interaction_resources(&context);
    bounded(group.dispose()).await.unwrap();
    assert_eq!(
        probe.events(),
        ["construct:executionWorld:1"],
        "a construct-phase failure registers no owner, so disposal emits nothing"
    );
    assert_no_interaction_resources(&context);
}

/// Negative control: `with_interaction` over `Families::unwired` must still
/// report the execution-world family as explicitly unsupported, not stage
/// silently, and must not publish the marker or fabricate an owner.
#[tokio::test]
async fn unwired_interaction_still_reports_execution_world_unsupported() {
    let families = Families::unwired().with_interaction();
    assert!(
        families.execution_world.layers_interaction(),
        "with_interaction must keep the wrapper over the unsupported source"
    );
    assert_eq!(families.execution_world.family(), Family::ExecutionWorld);

    let group = interaction_test_group();
    let context = group.context();
    let error = families
        .execution_world
        .stage(&context, &mut group.sink())
        .expect_err("unwired execution world must not stage through the wrapper");

    assert_eq!(
        error,
        RuntimeError::new(
            "Project runtime resource family executionWorld is not available in the native runtime yet (A6.9-pr3c)."
        )
    );
    assert!(context.owner(Family::ExecutionWorld).is_none());
    assert!(context.resource(Resource::ExecutionWorld).is_none());
    assert_no_interaction_resources(&context);
    bounded(group.dispose()).await.unwrap();
    assert_no_interaction_resources(&context);
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
        telemetry: None,
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
            Family::ContextStorage => families.context_storage = Arc::new(NoopSource(family)),
            Family::SessionTitleProvider => {
                families.session_title = Some(Arc::new(NoopSource(family)))
            }
            Family::McpProvider => families.mcp = Arc::new(NoopSource(family)),
            Family::MemoryBundle => families.memory = Arc::new(NoopSource(family)),
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
        (Family::ContextStorage, "A6.9-pr3d"),
        (
            Family::SessionTitleProvider,
            "A6.9-pr3d createNativeSessionTitleProvider",
        ),
        (Family::McpProvider, "A6.9-pr3c"),
        (Family::MemoryBundle, "A6.9-pr3d"),
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
            telemetry: None,
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
        );
        bounded(registry.dispose()).await.unwrap();
    }
}

fn plugin_test_registry(scratch: &Scratch) -> (ProjectRuntimeRegistry, PathBuf) {
    use crate::runtime::plugins::PluginRuntimeOptions;
    const CONFIG_YAML: &str = "schemaVersion: 1\nagent: {model: test/mock}\nmodel:\n  providers:\n    test:\n      protocol: openai\n      url: http://127.0.0.1:28794/v1\n      apiKey: mock\n      models:\n        mock: {}\n";

    let project_root = scratch.0.join("project");
    let pilot_home = scratch.0.join("pilot_home");
    std::fs::create_dir_all(project_root.join(".pilotdeck")).unwrap();
    std::fs::create_dir(&pilot_home).unwrap();
    std::fs::write(pilot_home.join("pilotdeck.yaml"), CONFIG_YAML).unwrap();
    std::fs::write(project_root.join(".pilotdeck/pilotdeck.yaml"), CONFIG_YAML).unwrap();
    // NativeConfigSource resolves its home from env, independently of the
    // resource pilot_home option; homedir is only the fallback for `~`.
    let mut env = Env::new();
    env.insert("PILOT_HOME".into(), pilot_home.to_str().unwrap().into());

    let plugin_options = PluginRuntimeOptions {
        project_root: project_root.clone(),
        pilot_home: pilot_home.clone(),
        builtin_skills_root: None,
        builtin_plugins: Vec::new(),
        builtin_plugins_enabled: std::collections::HashMap::new(),
    };

    // Keep the families under test native and supply recording fakes only
    // for the unrelated families whose native wiring has not landed yet.
    let probe = Arc::new(Probe::default());
    let other_family = |family| -> Arc<dyn ResourceFamilySource> {
        Arc::new(FakeSource {
            probe: Arc::clone(&probe),
            family,
            extra: no_extra,
            registers_owner: family != Family::ContextStorage,
            stages: true,
            publishes: &[],
        })
    };
    let families = Families {
        lsp: Some(other_family(Family::LspService)),
        context_storage: other_family(Family::ContextStorage),
        session_title: Some(other_family(Family::SessionTitleProvider)),
        memory: other_family(Family::MemoryBundle),
        ..Families::unwired()
    }
    .with_native_models()
    .with_tools_mcp()
    .with_plugins(plugin_options);

    let factory = ProjectResourcesFactory::new(ProjectResourcesOptions {
        pilot_home: pilot_home.clone(),
        cwd: scratch.0.to_path_buf(),
        env,
        homedir: Some(pilot_home),
        builtin_skills_root: None,
        now: Arc::new(|| SystemTime::UNIX_EPOCH),
        telemetry: None,
        config_version: None,
        runtime_profile_overrides: RuntimeProfileOverrides::default(),
        families,
    });
    let registry = ProjectRuntimeRegistry::new(&project_root, Arc::new(factory));
    (registry, project_root)
}

#[tokio::test]
async fn plugin_family_stages_successfully() {
    let scratch = Scratch::new();
    let (registry, project_root) = plugin_test_registry(&scratch);

    let runtime = registry
        .resolve(Some(&project_root))
        .expect("Factory staging with plugins should succeed");
    let lease = runtime.acquire().expect("Should acquire lease");
    assert_eq!(lease.runtime().project_root(), project_root);
    let context = lease
        .runtime()
        .resources()
        .get::<ProjectResources>()
        .expect("Factory resources should be published")
        .context();
    let plugin = context
        .resource(Resource::PluginRuntime)
        .expect("Native plugin runtime should be staged");
    assert!(
        plugin
            .as_any()
            .is::<crate::runtime::project::plugins::PluginResources>()
    );
    assert!(Arc::ptr_eq(
        &plugin,
        &context.owner(Family::PluginRuntime).unwrap()
    ));
    drop(lease);
    bounded(registry.dispose()).await.unwrap();
}

#[tokio::test]
async fn custom_router_registry_stages_after_plugins() {
    let scratch = Scratch::new();
    let (registry, project_root) = plugin_test_registry(&scratch);

    let runtime = registry
        .resolve(Some(&project_root))
        .expect("Factory staging with custom router should succeed");
    let lease = runtime.acquire().expect("Should acquire lease");

    assert_eq!(lease.runtime().project_root(), project_root);
    let context = lease
        .runtime()
        .resources()
        .get::<ProjectResources>()
        .expect("Factory resources should be published")
        .context();
    // The native custom router source requires the native plugin resource
    // to be available already; staging in the opposite order must fail.
    let routers = context
        .resource(Resource::RouterCustomRouters)
        .expect("Native custom router registry should be staged after plugins");
    assert!(
        routers
            .as_any()
            .is::<crate::runtime::project::custom_routers::CustomRouterResources>()
    );
    assert!(Arc::ptr_eq(
        &routers,
        &context.owner(Family::RouterCustomRouters).unwrap()
    ));

    drop(lease);
    bounded(registry.dispose()).await.unwrap();
}

/// Registry over the production factory with native model/tools/plugins and
/// the production interaction wrapper; only families whose native wiring has
/// not landed are recording fakes. The chain is the sibling of the plugin
/// coverage above: it exercises resolve -> publication -> lease -> dispose with
/// the marker present.
fn interaction_native_registry(
    scratch: &Scratch,
    probe: &Arc<Probe>,
) -> (ProjectRuntimeRegistry, PathBuf) {
    use crate::runtime::plugins::PluginRuntimeOptions;
    const CONFIG_YAML: &str = "schemaVersion: 1\nagent: {model: test/mock}\nmodel:\n  providers:\n    test:\n      protocol: openai\n      url: http://127.0.0.1:28794/v1\n      apiKey: mock\n      models:\n        mock: {}\n";

    let project_root = scratch.0.join("interaction-project");
    let pilot_home = scratch.0.join("interaction-pilot-home");
    std::fs::create_dir_all(project_root.join(".pilotdeck")).unwrap();
    std::fs::create_dir(&pilot_home).unwrap();
    std::fs::write(pilot_home.join("pilotdeck.yaml"), CONFIG_YAML).unwrap();
    std::fs::write(project_root.join(".pilotdeck/pilotdeck.yaml"), CONFIG_YAML).unwrap();
    let mut env = Env::new();
    env.insert("PILOT_HOME".into(), pilot_home.to_str().unwrap().into());

    let plugin_options = PluginRuntimeOptions {
        project_root: project_root.clone(),
        pilot_home: pilot_home.clone(),
        builtin_skills_root: None,
        builtin_plugins: Vec::new(),
        builtin_plugins_enabled: std::collections::HashMap::new(),
    };
    let other_family = |family| -> Arc<dyn ResourceFamilySource> {
        Arc::new(FakeSource {
            probe: Arc::clone(probe),
            family,
            extra: no_extra,
            registers_owner: family != Family::ContextStorage,
            stages: true,
            publishes: &[],
        })
    };
    let families = Families {
        lsp: Some(other_family(Family::LspService)),
        context_storage: other_family(Family::ContextStorage),
        session_title: Some(other_family(Family::SessionTitleProvider)),
        memory: other_family(Family::MemoryBundle),
        ..Families::unwired()
    }
    .with_native_models()
    .with_tools_mcp()
    .with_plugins(plugin_options)
    .with_interaction();

    let factory = ProjectResourcesFactory::new(ProjectResourcesOptions {
        pilot_home: pilot_home.clone(),
        cwd: scratch.0.to_path_buf(),
        env,
        homedir: Some(pilot_home),
        builtin_skills_root: None,
        now: Arc::new(|| SystemTime::UNIX_EPOCH),
        telemetry: None,
        config_version: None,
        runtime_profile_overrides: RuntimeProfileOverrides::default(),
        families,
    });
    let registry = ProjectRuntimeRegistry::new(&project_root, Arc::new(factory));
    (registry, project_root)
}

/// Positive registry control: the production factory stages the interaction
/// wrapper through the real registry, publishes one usable generation, and
/// disposes every fake owner exactly once.
#[tokio::test]
async fn interaction_registry_resolve_stages_and_disposes() {
    let scratch = Scratch::new();
    let probe = Arc::new(Probe::default());
    let (registry, project_root) = interaction_native_registry(&scratch, &probe);

    let runtime = registry
        .resolve(Some(&project_root))
        .expect("interaction-wrapped staging must publish a generation");
    let lease = runtime
        .acquire()
        .expect("the published generation is active");
    let context = lease
        .runtime()
        .resources()
        .get::<ProjectResources>()
        .expect("Factory resources should be published")
        .context();
    let interaction = context
        .resource(Resource::Interaction)
        .expect("the published generation must carry the interaction marker");
    let execution_world = context
        .resource(Resource::ExecutionWorld)
        .expect("the published generation must carry the execution world");
    assert!(
        !execution_world.as_any().is::<FakeHandle>(),
        "the native execution-world source must be staged under the wrapper"
    );
    assert!(Arc::ptr_eq(
        &context.owner(Family::ExecutionWorld).unwrap(),
        &execution_world
    ));
    assert!(!Arc::ptr_eq(&interaction, &execution_world));
    assert!(context.owner(Family::McpProvider).is_some());
    assert_eq!(
        context.profile().interaction.name,
        InteractionProfileName::Interactive,
        "the marker must not switch the config's default profile"
    );

    drop(lease);
    bounded(registry.dispose()).await.unwrap();
    assert_eq!(runtime.state().as_str(), "disposed");
    bounded(runtime.wait_disposed()).await.unwrap();
    let events = probe.events();
    for family in ["lspService", "sessionTitleProvider", "memoryBundle"] {
        assert_eq!(
            count_events(&events, &format!("dispose:{family}:")),
            1,
            "{family} must be disposed exactly once"
        );
    }
}

/// Registry over the production factory whose family constructors are the
/// recording fakes, with the production interaction wrapper layered over the
/// fake execution-world source. Only the wrapper is production here; the
/// fixture observes rollback of the wrapped owner and of later families.
fn interaction_fake_registry(scratch: &Scratch, probe: &Arc<Probe>) -> ProjectRuntimeRegistry {
    std::fs::create_dir_all(scratch.0.join("a/.pilotdeck")).unwrap();
    std::fs::write(
        scratch.0.join("a/.pilotdeck/pilotdeck.yaml"),
        PROJECT_CONFIGS[0].1,
    )
    .unwrap();
    std::fs::create_dir_all(scratch.0.join("config-home")).unwrap();
    std::fs::write(scratch.0.join("config-home/pilotdeck.yaml"), GLOBAL_YAML).unwrap();
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
        telemetry: None,
        config_version: None,
        runtime_profile_overrides: RuntimeProfileOverrides::default(),
        families: families(probe).with_interaction(),
    });
    ProjectRuntimeRegistry::new(scratch.0.join("fallback"), Arc::new(factory))
}

/// Negative registry control: a family failing after the interaction wrapper
/// was staged must not publish the generation. Already registered owners roll
/// back exactly once, and a retry stages a fresh generation rather than
/// consuming the failed one's capabilities. The marker itself is stateless and
/// has no dispose entry; only the wrapped execution-world owner is observed.
#[tokio::test]
async fn interaction_registry_rolls_back_failed_post_interaction_family() {
    let scratch = Scratch::new();
    let probe = Arc::new(Probe::default());
    *probe.fail.lock().unwrap() = Some((Family::MemoryBundle, Phase::Stage));
    let registry = interaction_fake_registry(&scratch, &probe);
    let root = scratch.0.join("a");

    let error = match registry.resolve(Some(&root)) {
        Err(error) => error,
        Ok(_) => panic!("a post-interaction family failure must fail the generation"),
    };
    assert_eq!(error, RuntimeError::new("stage failed:memoryBundle:12"));
    until(|| has_event(&probe.events(), "dispose:memoryBundle:12")).await;
    settle().await;
    let events = probe.events();
    assert_eq!(count_events(&events, "construct:executionWorld:"), 1);
    assert_eq!(count_events(&events, "stage:executionWorld:"), 1);
    assert_eq!(
        count_events(&events, "dispose:executionWorld:"),
        1,
        "the wrapped execution-world owner must roll back exactly once"
    );
    assert_eq!(count_events(&events, "dispose:mcpProvider:"), 1);
    assert_eq!(
        count_events(&events, "dispose:memoryBundle:"),
        1,
        "the failing family's registered owner must roll back exactly once"
    );

    // The failed generation was never published into the registry: a second
    // resolve stages every family again with fresh handles.
    let error = match registry.resolve(Some(&root)) {
        Err(error) => error,
        Ok(_) => panic!("the retry must fail until the injected failure is cleared"),
    };
    assert_eq!(error, RuntimeError::new("stage failed:memoryBundle:24"));
    until(|| has_event(&probe.events(), "dispose:memoryBundle:24")).await;
    settle().await;
    let events = probe.events();
    assert_eq!(count_events(&events, "construct:executionWorld:"), 2);
    assert_eq!(count_events(&events, "stage:executionWorld:"), 2);
    assert_eq!(count_events(&events, "dispose:executionWorld:"), 2);
    assert_eq!(
        count_events(&events, "dispose:executionWorld:6"),
        1,
        "the first failed generation's owner must not be disposed twice"
    );
    assert_eq!(count_events(&events, "dispose:executionWorld:18"), 1);

    // A subsequent complete generation is fresh: it owns new handles and is
    // the only one carrying the published marker.
    *probe.fail.lock().unwrap() = None;
    let runtime = registry
        .resolve(Some(&root))
        .expect("a complete generation must publish");
    let lease = runtime
        .acquire()
        .expect("the published generation is active");
    let context = lease
        .runtime()
        .resources()
        .get::<ProjectResources>()
        .expect("Factory resources should be published")
        .context();
    assert!(context.resource(Resource::Interaction).is_some());
    assert_eq!(
        handle_id(context.resource(Resource::ExecutionWorld)),
        "30",
        "the published generation must not reuse a failed generation's handles"
    );
    drop(lease);
    bounded(registry.dispose()).await.unwrap();
    assert_eq!(runtime.state().as_str(), "disposed");
    let events = probe.events();
    assert_eq!(count_events(&events, "dispose:executionWorld:30"), 1);
    assert_eq!(count_events(&events, "dispose:executionWorld:6"), 1);
    assert_eq!(count_events(&events, "dispose:executionWorld:18"), 1);
    assert_eq!(count_events(&events, "dispose:memoryBundle:36"), 1);
}

mod context_storage_tests {
    use super::*;
    use crate::host::modules::context::{ContextInput, ContextRuntime};
    use crate::runtime::project::context_storage::{
        ContextStorageFamilySource, ContextStorageResources, SessionContextStorage,
    };

    fn snapshot() -> Value {
        json!({"config": {
            "agent": {"model": {"provider": "custom", "model": "wide"}},
            "model": {"providers": {
                "other": {"models": {"wide": {"capabilities": {"maxContextTokens": 8192}}}},
                "custom": {"models": {
                    "small": {"capabilities": {"maxContextTokens": 4096}},
                    "wide": {"capabilities": {"maxContextTokens": 128000}}
                }}
            }}
        }})
    }

    fn default_profile() -> RuntimeProfile {
        RuntimeProfile::resolve(&JsValue::Undefined, &RuntimeProfileOverrides::default())
    }

    fn profile_for(config: &Value) -> RuntimeProfile {
        let agent = pilotdeck_protocol::parse_json(&config["config"]["agent"].to_string()).unwrap();
        RuntimeProfile::resolve(&agent, &RuntimeProfileOverrides::default())
    }

    fn stage_with(
        root: &Path,
        snapshot: Value,
        profile: RuntimeProfile,
        now: Arc<dyn Fn() -> std::time::SystemTime + Send + Sync>,
    ) -> ProjectResources {
        let group = ProjectResources::new(
            root.to_path_buf(),
            root.join("home"),
            None,
            crate::config::Env::new(),
            now,
            Arc::new(pilotdeck_protocol::parse_json(&snapshot.to_string()).unwrap()),
            profile,
        );
        ContextStorageFamilySource
            .stage(&group.context(), &mut group.sink())
            .unwrap();
        assert!(group.context().owner(Family::ContextStorage).is_none());
        group
    }

    fn stage(root: &Path, snapshot: Value) -> ProjectResources {
        stage_with(root, snapshot, default_profile(), Arc::new(|| UNIX_EPOCH))
    }

    fn input(value: Value) -> ContextInput {
        ContextInput {
            fields: pilotdeck_protocol::parse_json(&value.to_string())
                .unwrap()
                .as_object()
                .unwrap()
                .clone(),
            abort_signal: None,
            budget_evaluator: None,
        }
    }

    #[tokio::test]
    async fn context_storage_uses_selected_catalog_cap_and_nullish_override() {
        let scratch = Scratch::new();
        for (override_value, expected) in [
            (None, 128000.0),
            (Some(Value::Null), 128000.0),
            (Some(json!(64000)), 64000.0),
            (Some(json!(0)), 0.0),
        ] {
            let mut config = snapshot();
            if let Some(value) = override_value {
                config["config"]["agent"]["maxContextTokens"] = value;
            }
            let group = stage(&scratch.0, config);
            let resource = group
                .context()
                .resource(Resource::InstructionStorage)
                .unwrap();
            let storage = resource
                .as_any()
                .downcast_ref::<ContextStorageResources>()
                .unwrap();
            let session = storage.create_session(scratch.0.join("spill")).unwrap();
            let result = session
                .context_runtime
                .try_auto_compact(input(json!({})))
                .await
                .unwrap();
            assert_eq!(
                result
                    .get("snapshot")
                    .unwrap()
                    .get("maxContextTokens")
                    .unwrap()
                    .as_number(),
                Some(expected)
            );
        }
    }

    #[test]
    fn context_storage_missing_model_or_cap_does_not_invent_a_limit() {
        let scratch = Scratch::new();
        for missing in ["provider", "model", "cap"] {
            let mut config = snapshot();
            match missing {
                "provider" => {
                    config["config"]["agent"]["model"]["provider"] = json!("absent");
                    config["config"]["agent"]["maxContextTokens"] = json!(64000);
                }
                "model" => {
                    config["config"]["agent"]["model"]["model"] = json!("absent");
                    config["config"]["agent"]["maxContextTokens"] = json!(64000);
                }
                _ => {
                    config["config"]["model"]["providers"]["custom"]["models"]["wide"]["capabilities"] =
                        json!({})
                }
            }
            let group = stage(&scratch.0, config);
            let resource = group
                .context()
                .resource(Resource::InstructionStorage)
                .unwrap();
            let storage = resource
                .as_any()
                .downcast_ref::<ContextStorageResources>()
                .unwrap();
            assert!(
                storage.create_session(scratch.0.join("spill")).is_err(),
                "{missing}"
            );
        }
    }

    async fn spill_with(
        session: &SessionContextStorage,
        text: &str,
        turn_id: &str,
        tool_call_id: &str,
    ) -> JsValue {
        let result = session
            .context_runtime
            .apply_tool_results(input(json!({
                "turnId": turn_id,
                "messages": [],
                "toolResultMessage": {"role": "user", "content": [{
                    "type": "tool_result", "toolCallId": tool_call_id,
                    "content": [{"type": "text", "text": text}]
                }]}
            })))
            .await
            .unwrap();
        assert_eq!(result.get("diagnostics"), Some(&JsValue::Array(vec![])));
        let JsValue::Array(messages) = result.get("appendedMessages").unwrap() else {
            panic!("messages")
        };
        let JsValue::Array(content) = messages[0].get("content").unwrap() else {
            panic!("content")
        };
        assert_eq!(
            content[0].get("type"),
            Some(&"tool_result_reference".into())
        );
        content[0].clone()
    }

    async fn spill(session: &SessionContextStorage, text: &str) -> JsValue {
        spill_with(session, text, "same-turn", "same-call").await
    }

    fn text_field(value: &JsValue, field: &str) -> String {
        value
            .get(field)
            .unwrap()
            .as_string()
            .unwrap()
            .to_utf8()
            .unwrap()
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn context_storage_sessions_isolate_reused_ids_and_spill_files() {
        let scratch = Scratch::new();
        let group = stage(&scratch.0, snapshot());
        let resource = group
            .context()
            .resource(Resource::InstructionStorage)
            .unwrap();
        let spill_resource = group.context().resource(Resource::ToolResultSpill).unwrap();
        assert!(Arc::ptr_eq(&resource, &spill_resource));
        let storage = resource
            .as_any()
            .downcast_ref::<ContextStorageResources>()
            .unwrap();
        let dir = scratch.0.join(".pilotdeck/tool-results");
        let first = storage.create_session(dir.join("session-a")).unwrap();
        let second = storage.create_session(dir.join("session-b")).unwrap();
        assert!(!Arc::ptr_eq(
            &first.tool_result_budget,
            &second.tool_result_budget
        ));
        let first_text = "a".repeat(200_001);
        let second_text = "b".repeat(200_001);
        let barrier = Arc::new(tokio::sync::Barrier::new(2));
        let launch = |session: SessionContextStorage, text: String| {
            let barrier = barrier.clone();
            tokio::spawn(async move {
                barrier.wait().await;
                let reference = spill(&session, &text).await;
                (session, reference)
            })
        };
        let (first, second) = tokio::join!(
            launch(first, first_text.clone()),
            launch(second, second_text.clone())
        );
        let (first, first_ref) = first.unwrap();
        let (_, second_ref) = second.unwrap();
        for (reference, session_id, expected) in [
            (&first_ref, "session-a", &first_text),
            (&second_ref, "session-b", &second_text),
        ] {
            let path = PathBuf::from(text_field(reference, "path"));
            assert_eq!(path.parent(), Some(dir.join(session_id).as_path()));
            assert_eq!(std::fs::read_to_string(path).unwrap(), *expected);
            let alias = text_field(reference, "readFilePath");
            assert!(alias.starts_with(".pilotdeck/tool-results/refs/result-"));
            assert_eq!(
                std::fs::read_to_string(scratch.0.join(alias)).unwrap(),
                *expected
            );
        }
        assert_ne!(first_ref.get("path"), second_ref.get("path"));
        assert_ne!(
            first_ref.get("readFilePath"),
            second_ref.get("readFilePath")
        );
        // Reuse within the same context keeps the original cached replacement.
        assert_eq!(spill(&first, "small later result").await, first_ref);
    }

    /// #637: on a `worker_threads = 2` runtime, several sessions spilling
    /// large results through the real `apply_tool_results` path must not
    /// starve unrelated tasks on the same runtime. The slow-I/O probe blocks
    /// inside the original spill write/alias-copy path (the real exclusive
    /// open and write still run; the probe is not a sleep replacing the
    /// I/O), so before the `spawn_blocking` offload each spill occupied a
    /// worker for both stalls and the heartbeat below went silent for their
    /// whole duration. The pass/fail line separates real starvation from an
    /// isolated CI-noise gap (fix-r5, prompted by run 40184531435's single
    /// 185 ms outlier on an 11 ms median / 13 ms p99 timeline): failure
    /// needs one interval a full 500 ms controlled stall long — the exact
    /// minimum a worker held through a probe stall imposes; scheduling
    /// noise never blocks a worker — or near-stall gaps repeating; see
    /// `STARVATION_GAP_MS` for the full rationale. No threshold is relaxed
    /// to make this pass. On failure the diagnostics print every
    /// over-threshold gap with its index, plus max/median/p99, the tick
    /// count and the stalled opens.
    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn context_storage_spill_does_not_starve_same_runtime_tasks() {
        use crate::runtime::context::tool_budget::slow_spill_io_under;
        use std::{
            sync::{
                Arc,
                atomic::{AtomicBool, Ordering},
            },
            time::Instant,
        };

        const HEARTBEAT_MS: u64 = 10;
        const CONTROLLED_STALL_MS: u64 = 500;
        // #637 fix-r5: distinguish real starvation from an isolated CI-noise
        // gap instead of bounding the raw max. A worker running the spill
        // synchronously sits inside one uninterruptible controlled stall
        // (500 ms), and an interval straddling a stall is at least the whole
        // stall long — with four sessions chained two per worker the
        // pre-fix shape merged every stall into one ~3.1 s gap (fix-r5 red
        // control, 20/20 runs, `_tmp/.../fix-r5/heartbeat-red-20.log`; the
        // fix-r1 control measured 1.9 s). CI noise is the opposite shape:
        // the ubuntu flake (run 40184531435) had median 11 ms / p99 13 ms
        // with a single isolated 185 ms gap — scheduling jitter, no worker
        // blocked, and it cannot approach a stall because noise never
        // occupies a worker. Failure therefore requires the starvation
        // signature: one interval a full stall long, or stall-sized gaps
        // repeating (a worker ping-ponging through the four sessions'
        // spills). A single sub-stall gap is tolerated as noise, and the
        // full-stall arm keeps the exact structural minimum a blocked
        // worker imposes, so no threshold was widened past what real
        // starvation can be smaller than.
        const STARVATION_GAP_MS: u64 = CONTROLLED_STALL_MS * 4 / 5;

        let scratch = Scratch::new();
        let group = stage(&scratch.0, snapshot());
        let resource = group.context().resource(Resource::ToolResultSpill).unwrap();
        let storage = resource
            .as_any()
            .downcast_ref::<ContextStorageResources>()
            .unwrap();
        let dir = scratch.0.join(".pilotdeck/tool-results");
        let sessions: Vec<SessionContextStorage> = ["a", "b", "c", "d"]
            .into_iter()
            .map(|id| {
                storage
                    .create_session(dir.join(format!("session-{id}")))
                    .unwrap()
            })
            .collect();
        let slow = slow_spill_io_under(&scratch.0, Duration::from_millis(CONTROLLED_STALL_MS));

        let stop = Arc::new(AtomicBool::new(false));
        let heartbeat_stop = stop.clone();
        let heartbeat = tokio::spawn(async move {
            let mut intervals: Vec<Duration> = Vec::new();
            let mut last = Instant::now();
            while !heartbeat_stop.load(Ordering::SeqCst) {
                tokio::time::sleep(Duration::from_millis(HEARTBEAT_MS)).await;
                let now = Instant::now();
                intervals.push(now - last);
                last = now;
            }
            intervals
        });

        let texts: Vec<String> = ["a", "b", "c", "d"]
            .into_iter()
            .map(|c| c.repeat(200_001))
            .collect();
        // Four concurrent spill tasks, not two: with only one stealable task
        // queued, tokio work stealing will not take it from the busy worker's
        // queue, the two sessions serialize on one worker and the starved
        // heartbeat keeps the other — the red case would be flaky. With a
        // four-way barrier the idle worker always finds stealable work, so
        // both workers deterministically hold synchronous spill calls.
        let barrier = Arc::new(tokio::sync::Barrier::new(texts.len()));
        let mut launches = Vec::new();
        for (session, text) in sessions.into_iter().zip(texts.iter().cloned()) {
            let barrier = barrier.clone();
            launches.push(tokio::spawn(async move {
                barrier.wait().await;
                let reference = spill(&session, &text).await;
                (session, reference)
            }));
        }
        let mut references = Vec::new();
        for launch in launches {
            let (_session, reference) = launch.await.unwrap();
            references.push(reference);
        }
        let stalled_opens = slow.fires();
        drop(slow);
        stop.store(true, Ordering::SeqCst);
        let intervals = heartbeat.await.unwrap();
        let ticks = intervals.len();
        let max_gap = intervals.iter().max().copied().unwrap_or(Duration::ZERO);
        let mut sorted: Vec<u128> = intervals.iter().map(|gap| gap.as_millis()).collect();
        sorted.sort_unstable();
        let (median_ms, p99_ms) = if sorted.is_empty() {
            (0, 0)
        } else {
            (
                sorted[sorted.len() / 2],
                sorted[(sorted.len() * 99).div_ceil(100).min(sorted.len() - 1)],
            )
        };
        // Every interval at or above the starvation threshold, with its
        // index, so a failure shows exactly which gaps crossed and when —
        // one isolated entry is CI noise, a run of them is a blocked worker.
        let starved: Vec<(usize, u128)> = intervals
            .iter()
            .enumerate()
            .filter(|(_, gap)| gap.as_millis() >= STARVATION_GAP_MS as u128)
            .map(|(index, gap)| (index, gap.as_millis()))
            .collect();
        // The starvation signature (see STARVATION_GAP_MS): a single gap a
        // full stall long — only a worker held through an entire probe
        // stall can produce it — or the near-stall gaps repeating.
        let starved_a_full_stall = starved
            .iter()
            .any(|(_, ms)| *ms >= CONTROLLED_STALL_MS as u128);
        let starvation = starved_a_full_stall || starved.len() >= 2;

        // The real spill files and aliases landed with the expected contents.
        for ((id, reference), expected) in ["a", "b", "c", "d"].iter().zip(&references).zip(&texts)
        {
            let session_id = format!("session-{id}");
            let path = PathBuf::from(text_field(reference, "path"));
            assert_eq!(path.parent(), Some(dir.join(&session_id).as_path()));
            assert_eq!(std::fs::read_to_string(path).unwrap(), *expected);
            let alias = text_field(reference, "readFilePath");
            assert_eq!(
                std::fs::read_to_string(scratch.0.join(alias)).unwrap(),
                *expected
            );
        }
        // Each session stalled one spill write and one alias-copy open.
        assert!(
            stalled_opens >= 8,
            "the slow-I/O probe must reach the real open path (fires={stalled_opens})"
        );
        // The heartbeat really ran (P3-3): a stopped-too-early loop with no
        // intervals must not pass vacuously.
        assert!(
            ticks >= 5,
            "heartbeat barely ran (ticks={ticks}, stalled opens={stalled_opens})"
        );
        assert!(
            !starvation,
            "heartbeat starved: {} interval(s) >= {STARVATION_GAP_MS}ms (full-stall \
             gap: {starved_a_full_stall}) while four sessions spilled \
             (starved_gaps={starved:?}, max={max_gap:?}, ticks={ticks}, \
             stalled opens={stalled_opens}, median={median_ms}ms, p99={p99_ms}ms, \
             intervals_ms={sorted:?})",
            starved.len()
        );
    }

    #[test]
    fn context_storage_scoped_contexts_accept_same_execution_spill_root() {
        let scratch = Scratch::new();
        let group = stage(&scratch.0, snapshot());
        let resource = group.context().resource(Resource::ToolResultSpill).unwrap();
        let storage = resource
            .as_any()
            .downcast_ref::<ContextStorageResources>()
            .unwrap();
        let custom_root = scratch.0.join("execution-spill");
        let first = storage.create_session(&custom_root).unwrap();
        let second = storage.create_session(&custom_root).unwrap();
        assert!(!Arc::ptr_eq(
            &first.tool_result_budget,
            &second.tool_result_budget
        ));
        assert_eq!(
            first.tool_result_budget.tool_results_dir(),
            second.tool_result_budget.tool_results_dir()
        );
        assert!(
            !custom_root.exists(),
            "composition must not perform spill I/O"
        );
    }

    // project_root and the default/overridden retrieval timeout are exercised
    // through normalized config and the real memory resource/session chain in
    // factory_memory_tests.rs (#639).

    fn prepare_input(cwd: &Path) -> ContextInput {
        let cwd = cwd.to_string_lossy().into_owned();
        input(json!({
            "sessionId": "session-1",
            "turnId": "turn-1",
            "cwd": cwd,
            "provider": "custom",
            "model": "wide",
            "permissionMode": "default",
            "messages": [{"role": "user", "content": [{"type": "text", "text": "hello"}]}],
            "tools": []
        }))
    }

    #[tokio::test]
    async fn context_storage_profile_surface_changes_user_context_placement() {
        let scratch = Scratch::new();
        for (surface, in_system_prompt) in [("system_prompt", true), ("user_message", false)] {
            let mut config = snapshot();
            config["config"]["agent"]["runtimeContextSurface"] = json!(surface);
            let profile = profile_for(&config);
            let group = stage_with(&scratch.0, config, profile, Arc::new(|| UNIX_EPOCH));
            let resource = group
                .context()
                .resource(Resource::InstructionStorage)
                .unwrap();
            let storage = resource
                .as_any()
                .downcast_ref::<ContextStorageResources>()
                .unwrap();
            let session = storage.create_session(scratch.0.join("spill")).unwrap();
            let result = session
                .context_runtime
                .prepare_for_model(prepare_input(&scratch.0))
                .await
                .unwrap();
            let system_prompt = text_field(&result, "systemPrompt");
            assert_eq!(
                system_prompt.contains("<user-context>"),
                in_system_prompt,
                "{surface}"
            );
            let JsValue::Array(messages) = result.get("messages").unwrap() else {
                panic!("messages")
            };
            if in_system_prompt {
                assert_eq!(messages.len(), 1, "{surface}");
            } else {
                assert_eq!(messages.len(), 2, "{surface}");
                assert_eq!(messages[0].get("role"), Some(&"user".into()), "{surface}");
                assert_eq!(
                    messages[0].get("metadata").unwrap().get("purpose"),
                    Some(&"runtime_context".into()),
                    "{surface}"
                );
                let JsValue::Array(content) = messages[0].get("content").unwrap() else {
                    panic!("content")
                };
                let text = content[0]
                    .get("text")
                    .unwrap()
                    .as_string()
                    .unwrap()
                    .to_utf8()
                    .unwrap();
                assert!(
                    text.contains("<runtime-context name=\"pilotdeck:user-context:0\">"),
                    "{surface}"
                );
                assert!(text.contains("<user-context>"), "{surface}");
            }
        }
    }

    #[tokio::test]
    async fn context_storage_resource_clock_reaches_prompt_environment() {
        let scratch = Scratch::new();
        let clock = UNIX_EPOCH + Duration::from_secs(1_749_484_800);
        let group = stage_with(
            &scratch.0,
            snapshot(),
            default_profile(),
            Arc::new(move || clock),
        );
        let resource = group
            .context()
            .resource(Resource::InstructionStorage)
            .unwrap();
        let storage = resource
            .as_any()
            .downcast_ref::<ContextStorageResources>()
            .unwrap();
        let session = storage.create_session(scratch.0.join("spill")).unwrap();
        let result = session
            .context_runtime
            .prepare_for_model(prepare_input(&scratch.0))
            .await
            .unwrap();
        let prompt = text_field(&result, "systemPrompt");
        assert!(
            prompt.contains("<environment>\nnow: 2025-06-09\n</environment>"),
            "{prompt}"
        );
        assert!(!prompt.contains("now: 1970-01-01"), "{prompt}");
    }

    #[tokio::test]
    async fn context_storage_discovers_project_and_user_instructions() {
        let scratch = Scratch::new();
        let root = scratch.0.clone();
        std::fs::create_dir_all(root.join("home")).unwrap();
        std::fs::create_dir_all(root.join("nested")).unwrap();
        std::fs::write(root.join("PILOTDECK.md"), "PROJECT-INSTRUCTION").unwrap();
        std::fs::write(root.join("home/PILOTDECK.md"), "USER-INSTRUCTION").unwrap();
        std::fs::write(root.join("nested/PILOTDECK.md"), "NESTED-INSTRUCTION").unwrap();

        let group = stage(&root, snapshot());
        let resource = group
            .context()
            .resource(Resource::InstructionStorage)
            .unwrap();
        let storage = resource
            .as_any()
            .downcast_ref::<ContextStorageResources>()
            .unwrap();
        let session = storage.create_session(root.join("spill")).unwrap();
        let result = session
            .context_runtime
            .prepare_for_model(prepare_input(&root))
            .await
            .unwrap();
        let prompt = text_field(&result, "systemPrompt");
        assert!(prompt.contains("PROJECT-INSTRUCTION"), "{prompt}");
        assert!(prompt.contains("USER-INSTRUCTION"), "{prompt}");
        assert!(
            !prompt.contains("NESTED-INSTRUCTION"),
            "discovery cwd is the project root, not an arbitrary descendant: {prompt}"
        );
        let JsValue::Array(layers) = result
            .get("materialization")
            .unwrap()
            .get("instructionLayers")
            .unwrap()
        else {
            panic!("instruction layers")
        };
        let project_path = root.join("PILOTDECK.md").to_string_lossy().into_owned();
        let project_layer = layers
            .iter()
            .find(|layer| text_field(layer, "path") == project_path)
            .unwrap();
        assert_eq!(text_field(project_layer, "scope"), "project");
        let user_path = root
            .join("home/PILOTDECK.md")
            .to_string_lossy()
            .into_owned();
        let user_layer = layers
            .iter()
            .find(|layer| text_field(layer, "path") == user_path)
            .unwrap();
        assert_eq!(text_field(user_layer, "scope"), "user");
    }

    #[tokio::test]
    async fn context_storage_real_normalized_config_creates_session_from_model_defaults() {
        let scratch = Scratch::new();
        let registry = registry_for_yaml(
            &scratch,
            CONFIG_YAML_NO_MEMORY,
            silent_families().with_context_memory(),
        );
        let root = scratch.0.join("project");
        let runtime = registry
            .resolve(Some(&root))
            .expect("the real normalized config must publish a generation");
        let lease = runtime
            .acquire()
            .expect("the published generation is active");
        let context = lease
            .runtime()
            .resources()
            .get::<ProjectResources>()
            .expect("Factory resources should be published")
            .context();
        let resource = context
            .resource(Resource::InstructionStorage)
            .expect("context storage must publish InstructionStorage");
        let storage = resource
            .as_any()
            .downcast_ref::<ContextStorageResources>()
            .unwrap();
        // `models: {mock: {}}` has no explicit maxContextTokens; the normalized
        // protocol default (openai: 128000) is the production value.
        let session = storage
            .create_session(scratch.0.join("normalized-spill"))
            .expect("protocol capabilities must supply maxContextTokens");
        let result = session
            .context_runtime
            .try_auto_compact(input(json!({})))
            .await
            .unwrap();
        assert_eq!(
            result
                .get("snapshot")
                .unwrap()
                .get("maxContextTokens")
                .unwrap()
                .as_number(),
            Some(128000.0)
        );
        drop(lease);
        bounded(registry.dispose()).await.unwrap();
    }

    #[tokio::test]
    async fn context_storage_shared_directory_requires_unique_scoped_keys() {
        let scratch = Scratch::new();
        let group = stage(&scratch.0, snapshot());
        let resource = group.context().resource(Resource::ToolResultSpill).unwrap();
        let storage = resource
            .as_any()
            .downcast_ref::<ContextStorageResources>()
            .unwrap();
        let dir = scratch.0.join(".pilotdeck/tool-results");
        let first = storage.create_session(&dir).unwrap();
        let second = storage.create_session(&dir).unwrap();
        let first_text = "a".repeat(200_001);
        let second_text = "b".repeat(200_001);

        let first_ref = spill_with(&first, &first_text, "t1", "c1").await;
        let second_ref = spill_with(&second, &second_text, "t1", "c1").await;

        let path = PathBuf::from(text_field(&first_ref, "path"));
        assert_eq!(second_ref.get("path"), first_ref.get("path"));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), first_text);
        // The second preview describes the second input, but its record points
        // at the first context's file; that file was never replaced.
        let preview = text_field(&second_ref, "preview");
        assert!(preview.starts_with('b'));
        assert!(!preview.contains('a'));
        assert_ne!(preview, first_text);
        let alias = text_field(&second_ref, "readFilePath");
        assert_ne!(alias, text_field(&first_ref, "readFilePath"));
        assert_eq!(
            std::fs::read_to_string(scratch.0.join(alias)).unwrap(),
            first_text
        );
    }

    #[tokio::test]
    async fn context_storage_shared_directory_requires_unique_sanitized_filenames() {
        let scratch = Scratch::new();
        let group = stage(&scratch.0, snapshot());
        let resource = group.context().resource(Resource::ToolResultSpill).unwrap();
        let storage = resource
            .as_any()
            .downcast_ref::<ContextStorageResources>()
            .unwrap();
        let dir = scratch.0.join(".pilotdeck/tool-results");
        let first = storage.create_session(&dir).unwrap();
        let second = storage.create_session(&dir).unwrap();
        let first_text = "a".repeat(200_001);
        let second_text = "b".repeat(200_001);

        // These raw tool-call ids differ but both sanitize to `c-1`, so the
        // shared spill file is `t1-c-1.txt` for both contexts.
        let first_ref = spill_with(&first, &first_text, "t1", "c/1").await;
        let second_ref = spill_with(&second, &second_text, "t1", "c-1").await;

        let path = PathBuf::from(text_field(&first_ref, "path"));
        assert_eq!(second_ref.get("path"), first_ref.get("path"));
        assert_eq!(
            path.file_name().and_then(|name| name.to_str()),
            Some("t1-c-1.txt")
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), first_text);
        // As with an identical raw key, the second preview describes the
        // second input while the persisted file and read alias still hold the
        // first context's body.
        let preview = text_field(&second_ref, "preview");
        assert!(preview.starts_with('b'));
        assert!(!preview.contains('a'));
        assert_ne!(preview, first_text);
        let alias = text_field(&second_ref, "readFilePath");
        assert_ne!(alias, text_field(&first_ref, "readFilePath"));
        assert_eq!(
            std::fs::read_to_string(scratch.0.join(alias)).unwrap(),
            first_text
        );
    }

    #[tokio::test]
    async fn context_storage_same_budget_sanitize_equal_ids_return_cached_record() {
        let scratch = Scratch::new();
        let group = stage(&scratch.0, snapshot());
        let resource = group.context().resource(Resource::ToolResultSpill).unwrap();
        let storage = resource
            .as_any()
            .downcast_ref::<ContextStorageResources>()
            .unwrap();
        let dir = scratch.0.join(".pilotdeck/tool-results");
        let session = storage.create_session(&dir).unwrap();
        let first_text = "a".repeat(200_001);
        let second_text = "b".repeat(200_001);
        let refs_entries = || {
            let mut names = std::fs::read_dir(dir.join("refs"))
                .unwrap()
                .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
                .collect::<Vec<_>>();
            names.sort();
            names
        };

        // `c/1` and `c-1` sanitize to the same scoped key `t1-c-1` within one
        // budget, so the second commit must return the first cached record.
        let first_ref = spill_with(&session, &first_text, "t1", "c/1").await;
        let aliases_after_first = refs_entries();
        let second_ref = spill_with(&session, &second_text, "t1", "c-1").await;

        assert_eq!(second_ref, first_ref);
        assert_eq!(text_field(&second_ref, "toolCallId"), "c/1");
        assert_eq!(
            text_field(&second_ref, "preview"),
            text_field(&first_ref, "preview")
        );
        assert!(text_field(&second_ref, "preview").starts_with('a'));

        let path = PathBuf::from(text_field(&first_ref, "path"));
        assert_eq!(
            path.file_name().and_then(|name| name.to_str()),
            Some("t1-c-1.txt")
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), first_text);
        let alias = text_field(&first_ref, "readFilePath");
        assert_eq!(
            std::fs::read_to_string(scratch.0.join(alias)).unwrap(),
            first_text
        );
        // A miss keeps `a` (write-if-absent), adds an alias and a new record; a hit writes nothing.
        assert_eq!(refs_entries(), aliases_after_first);
        assert_eq!(aliases_after_first.len(), 1);
    }

    #[tokio::test]
    async fn context_storage_shared_directory_distinguishes_unique_scoped_keys() {
        let scratch = Scratch::new();
        let group = stage(&scratch.0, snapshot());
        let resource = group.context().resource(Resource::ToolResultSpill).unwrap();
        let storage = resource
            .as_any()
            .downcast_ref::<ContextStorageResources>()
            .unwrap();
        let dir = scratch.0.join(".pilotdeck/tool-results");
        let first = storage.create_session(&dir).unwrap();
        let second = storage.create_session(&dir).unwrap();
        let first_text = "a".repeat(200_001);
        let second_text = "b".repeat(200_001);

        let first_ref = spill_with(&first, &first_text, "t1", "c1").await;
        let second_ref = spill_with(&second, &second_text, "t1", "c2").await;

        for (reference, expected) in [
            (&first_ref, first_text.as_str()),
            (&second_ref, second_text.as_str()),
        ] {
            let path = PathBuf::from(text_field(reference, "path"));
            assert!(path.starts_with(&dir));
            assert_eq!(std::fs::read_to_string(&path).unwrap(), expected);
            let alias = text_field(reference, "readFilePath");
            assert_eq!(
                std::fs::read_to_string(scratch.0.join(alias)).unwrap(),
                expected
            );
        }
        assert_ne!(first_ref.get("path"), second_ref.get("path"));
        assert_ne!(
            first_ref.get("readFilePath"),
            second_ref.get("readFilePath")
        );
    }
}

mod memory_bundle_tests {
    use super::*;
    use crate::host::Cancellation;
    use crate::runtime::context::memory::{
        MemoryFuture as ContextMemoryFuture, MemoryResolver, MemoryRetrieveInput,
        MemoryRetrieveResult,
    };
    use crate::runtime::memory::{
        MemoryBundleError, MemoryCaptureStrategy, MemoryConfig, MemoryDisposeFn,
        MemoryFuture as MaintenanceMemoryFuture, MemoryMaintenanceService, MemoryManagementService,
        MemoryProviderError, MemoryProviderFactoryFn, MemoryScheduleConfig,
        NativeMemoryProviderOptions, NativeMemoryService, NativeProjectMemoryProvider,
        NativeProviderFactoryFn, ProjectMemoryBundleOptions, ProjectMemoryProvider,
    };
    use crate::runtime::project::memory_bundle::{
        MemoryBundleFamilySource, MemoryBundleResource, MemoryMaintenanceResource,
        MemoryManagementResource, MemoryResolverResource,
    };
    use crate::runtime::project::{RuntimeResources, TelemetryClient};
    use pilotdeck_protocol::JsString;
    use std::sync::atomic::{AtomicBool, AtomicUsize};

    const MEMORY_YAML: &str = "schemaVersion: 1\nagent: {model: test/mock}\nmodel:\n  providers:\n    test:\n      protocol: openai\n      url: http://127.0.0.1:28794/v1\n      apiKey: mock\n      models:\n        mock: {}\nmemory:\n  enabled: true\n  provider: edgeclaw\n";

    fn memory_snapshot() -> Value {
        json!({"config": {
            "agent": {"model": {"id": "test/mock", "provider": "test", "model": "mock"}},
            "model": {"providers": {"test": {
                "id": "test",
                "protocol": "openai",
                "url": "http://127.0.0.1:28794/v1",
                "apiKey": "test-only-key",
                "headers": {},
                "models": {"mock": {}}
            }}},
            "memory": {
                "enabled": true,
                "provider": "edgeclaw",
                "rootDir": "/normalized/root",
                "captureStrategy": "full_session",
                "includeAssistant": false,
                "maxMessageChars": 321,
                "retrievalTimeoutMs": 42,
                "model": "test/mem",
                "apiType": "openai-completions",
                "schedule": {
                    "reasoningMode": "accuracy_first",
                    "autoIndexIntervalMinutes": 5,
                    "autoDreamIntervalMinutes": 9
                },
                "heartbeatBatchSize": 7
            }
        }})
    }

    fn expected_memory_config() -> MemoryConfig {
        MemoryConfig {
            enabled: true,
            provider: JsString::from("edgeclaw"),
            root_dir: JsString::from("/normalized/root"),
            capture_strategy: MemoryCaptureStrategy::FullSession,
            include_assistant: false,
            max_message_chars: Some(321.0),
            retrieval_timeout_ms: Some(42.0),
            model: Some(JsString::from("test/mem")),
            api_type: Some(JsString::from("openai-completions")),
            schedule: Some(MemoryScheduleConfig {
                reasoning_mode: Some(JsString::from("accuracy_first")),
                auto_index_interval_minutes: Some(5.0),
                auto_dream_interval_minutes: Some(9.0),
            }),
            heartbeat_batch_size: Some(7.0),
        }
    }

    fn memory_group(root: &Path, snapshot: Value) -> ProjectResources {
        memory_group_with(root, snapshot, Arc::new(|| UNIX_EPOCH), None)
    }

    fn memory_group_with(
        root: &Path,
        snapshot: Value,
        now: Now,
        telemetry: Option<TelemetryClient>,
    ) -> ProjectResources {
        ProjectResources::new(
            root.to_path_buf(),
            root.join("home"),
            None,
            crate::config::Env::new(),
            now,
            Arc::new(pilotdeck_protocol::parse_json(&snapshot.to_string()).unwrap()),
            RuntimeProfile::resolve(&JsValue::Undefined, &RuntimeProfileOverrides::default()),
        )
        .with_telemetry(telemetry)
    }

    fn stage_memory(source: &MemoryBundleFamilySource, group: &ProjectResources) -> RuntimeResult {
        source.stage(&group.context(), &mut group.sink())
    }

    /// Opaque telemetry sentinel: proves the exact host object crosses the
    /// stage boundary even though no native consumer calls it yet.
    #[derive(Debug)]
    struct ProbeTelemetry(&'static str);

    /// One provider implementing all three published surfaces; the counters
    /// prove the real handles reach their configured implementations.
    #[derive(Default)]
    struct FakeMemory {
        retrieve_calls: AtomicUsize,
        maintenance_calls: AtomicUsize,
        list_calls: AtomicUsize,
        clear_calls: AtomicUsize,
        clear_session_calls: AtomicUsize,
        dispose_calls: AtomicUsize,
        close_calls: AtomicUsize,
        retrieve_input: Mutex<Option<(String, String, String)>>,
    }

    impl MemoryResolver for FakeMemory {
        fn retrieve(&self, input: MemoryRetrieveInput) -> ContextMemoryFuture {
            self.retrieve_calls.fetch_add(1, Ordering::SeqCst);
            *self.retrieve_input.lock().unwrap() = Some((
                input.query.clone(),
                input.session_id.clone(),
                input.project_root.clone(),
            ));
            Box::pin(async move {
                Ok(MemoryRetrieveResult {
                    system_context: Some("remembered context".into()),
                    diagnostics: Vec::new(),
                })
            })
        }
    }

    impl MemoryMaintenanceService for FakeMemory {
        fn run_due_scheduled_maintenance(&self, _reason: &str) -> MaintenanceMemoryFuture<'_, ()> {
            self.maintenance_calls.fetch_add(1, Ordering::SeqCst);
            Box::pin(async { Ok(()) })
        }
    }

    impl MemoryManagementService for FakeMemory {
        fn list(&self, _limit: usize) -> Result<Vec<JsValue>, MemoryProviderError> {
            self.list_calls.fetch_add(1, Ordering::SeqCst);
            Ok(vec![JsValue::from("remembered")])
        }
        fn clear(&self, _scope: &str) -> Result<(), MemoryProviderError> {
            self.clear_calls.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
        fn clear_session(&self, _session_key: &JsString) -> Result<(), MemoryProviderError> {
            self.clear_session_calls.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }

    impl NativeMemoryService for FakeMemory {
        fn run_due_scheduled_maintenance(&self, _reason: &str) -> MaintenanceMemoryFuture<'_, ()> {
            self.maintenance_calls.fetch_add(1, Ordering::SeqCst);
            Box::pin(async { Ok(()) })
        }
        fn list(&self, _limit: usize) -> Result<Vec<JsValue>, MemoryProviderError> {
            self.list_calls.fetch_add(1, Ordering::SeqCst);
            Ok(vec![JsValue::from("remembered")])
        }
        fn clear(&self, _scope: &str) -> Result<(), MemoryProviderError> {
            self.clear_calls.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
        fn clear_session(&self, _session_key: &JsString) -> Result<(), MemoryProviderError> {
            self.clear_session_calls.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
        fn close(&self) {
            self.close_calls.fetch_add(1, Ordering::SeqCst);
        }
    }

    fn disposer(provider: &Arc<FakeMemory>) -> MemoryDisposeFn {
        let provider = Arc::clone(provider);
        Arc::new(move || {
            let provider = Arc::clone(&provider);
            Box::pin(async move {
                provider.dispose_calls.fetch_add(1, Ordering::SeqCst);
                Ok(())
            })
        })
    }

    fn injected_provider(provider: &Arc<FakeMemory>) -> ProjectMemoryProvider {
        let memory: Arc<dyn MemoryResolver> = provider.clone();
        let maintenance: Arc<dyn MemoryMaintenanceService> = provider.clone();
        let management: Arc<dyn MemoryManagementService> = provider.clone();
        ProjectMemoryProvider {
            memory,
            maintenance: Some(maintenance),
            management: Some(management),
            dispose: Some(disposer(provider)),
        }
    }

    #[tokio::test]
    async fn memory_factory_injects_provider_and_publishes_three_handles() {
        let scratch = Scratch::new();
        let telemetry: TelemetryClient = Arc::new(ProbeTelemetry("telemetry-sentinel"));
        let group = memory_group_with(
            &scratch.0.join("project"),
            memory_snapshot(),
            Arc::new(|| UNIX_EPOCH + Duration::from_secs(42)),
            Some(Arc::clone(&telemetry)),
        );
        let provider = Arc::new(FakeMemory::default());
        let captured: Arc<Mutex<Option<ProjectMemoryBundleOptions>>> = Arc::new(Mutex::new(None));
        let captured_in_factory = Arc::clone(&captured);
        let provider_in_factory = Arc::clone(&provider);
        let factory: MemoryProviderFactoryFn = Arc::new(move |options| {
            *captured_in_factory.lock().unwrap() = Some(options);
            Ok(Some(injected_provider(&provider_in_factory)))
        });
        let source = MemoryBundleFamilySource::default().with_provider_factory(factory);

        stage_memory(&source, &group).expect("an injected provider must stage");

        let options = captured
            .lock()
            .unwrap()
            .clone()
            .expect("the provider factory must receive the bundle options");
        assert_eq!(options.project_root, scratch.0.join("project"));
        assert_eq!(
            options.config,
            Some(expected_memory_config()),
            "the normalized memory config must reach the provider factory"
        );
        assert_eq!(options.agent_model.as_deref(), Some("test/mock"));
        let model_config = options
            .model_config
            .as_ref()
            .expect("the normalized model config must reach the provider factory");
        let provider_entry = model_config
            .providers
            .get("test")
            .expect("the configured provider entry must survive");
        assert_eq!(
            provider_entry.protocol.as_ref(),
            Some(&JsString::from("openai"))
        );
        assert_eq!(
            provider_entry.url.as_ref(),
            Some(&JsString::from("http://127.0.0.1:28794/v1"))
        );
        assert_eq!(
            provider_entry.api_key.as_ref(),
            Some(&JsString::from("test-only-key"))
        );
        assert_eq!(
            options
                .now
                .as_ref()
                .expect("the project clock must be passed")(),
            UNIX_EPOCH + Duration::from_secs(42),
            "the exact project clock must reach the provider factory"
        );
        assert_eq!(
            options
                .telemetry
                .as_ref()
                .expect("the telemetry client must be passed")
                .downcast_ref::<ProbeTelemetry>()
                .map(|telemetry| telemetry.0),
            Some("telemetry-sentinel"),
            "the exact telemetry object must cross the stage boundary"
        );
        assert!(options.provider_factory.is_some());
        assert!(options.create_provider.is_none());

        let context = group.context();
        assert!(
            context
                .owner(Family::MemoryBundle)
                .expect("the bundle owner is registered before staging")
                .as_any()
                .is::<MemoryBundleResource>(),
            "the owner must be the ProjectMemoryBundle wrapper"
        );
        let memory = context.resource(Resource::Memory).unwrap();
        let service = context.resource(Resource::MemoryService).unwrap();
        let management = context.resource(Resource::MemoryManagement).unwrap();
        let memory = memory
            .as_any()
            .downcast_ref::<MemoryResolverResource>()
            .unwrap();
        let service = service
            .as_any()
            .downcast_ref::<MemoryMaintenanceResource>()
            .unwrap();
        let management = management
            .as_any()
            .downcast_ref::<MemoryManagementResource>()
            .unwrap();
        let expected_memory: Arc<dyn MemoryResolver> = provider.clone();
        let expected_maintenance: Arc<dyn MemoryMaintenanceService> = provider.clone();
        let expected_management: Arc<dyn MemoryManagementService> = provider.clone();
        assert!(Arc::ptr_eq(memory.resolver(), &expected_memory));
        assert!(Arc::ptr_eq(service.maintenance(), &expected_maintenance));
        assert!(Arc::ptr_eq(management.management(), &expected_management));

        let retrieved = memory
            .resolver()
            .retrieve(MemoryRetrieveInput {
                query: "where".into(),
                session_id: "session-1".into(),
                project_root: scratch.0.join("project").to_string_lossy().into_owned(),
                recent_messages: Vec::new(),
                signal: Cancellation::default(),
            })
            .await
            .unwrap();
        assert_eq!(
            retrieved.system_context.as_deref(),
            Some("remembered context")
        );
        service
            .maintenance()
            .run_due_scheduled_maintenance("manual")
            .await
            .unwrap();
        assert_eq!(management.management().list(10).unwrap().len(), 1);
        management.management().clear("current_project").unwrap();
        management
            .management()
            .clear_session(&JsString::from("session-1"))
            .unwrap();
        assert_eq!(provider.retrieve_calls.load(Ordering::SeqCst), 1);
        assert_eq!(provider.maintenance_calls.load(Ordering::SeqCst), 1);
        assert_eq!(provider.list_calls.load(Ordering::SeqCst), 1);
        assert_eq!(provider.clear_calls.load(Ordering::SeqCst), 1);
        assert_eq!(provider.clear_session_calls.load(Ordering::SeqCst), 1);
        assert_eq!(
            provider.retrieve_input.lock().unwrap().as_ref(),
            Some(&(
                "where".into(),
                "session-1".into(),
                scratch.0.join("project").to_string_lossy().into_owned()
            ))
        );

        bounded(group.dispose()).await.unwrap();
        bounded(group.dispose()).await.unwrap();
        assert_eq!(
            provider.dispose_calls.load(Ordering::SeqCst),
            1,
            "the provider disposer must run exactly once across repeated disposal"
        );
    }

    #[tokio::test]
    async fn memory_factory_without_provider_publishes_no_resources() {
        let scratch = Scratch::new();
        let group = memory_group(&scratch.0.join("project"), memory_snapshot());
        let factory: MemoryProviderFactoryFn = Arc::new(|_| Ok(None));
        let source = MemoryBundleFamilySource::default().with_provider_factory(factory);
        stage_memory(&source, &group).expect("an absent provider is a valid stage");
        let context = group.context();
        assert!(context.owner(Family::MemoryBundle).is_some());
        assert!(context.resource(Resource::Memory).is_none());
        assert!(context.resource(Resource::MemoryService).is_none());
        assert!(context.resource(Resource::MemoryManagement).is_none());
        bounded(group.dispose()).await.unwrap();
    }

    #[tokio::test]
    async fn memory_factory_error_keeps_js_shape_and_registered_owner() {
        let scratch = Scratch::new();
        let group = memory_group(&scratch.0.join("project"), memory_snapshot());
        let message = JsString::from_utf16(
            "provider exploded "
                .encode_utf16()
                .chain([0xD800])
                .collect::<Vec<_>>(),
        );
        let thrown = message.clone();
        let factory: MemoryProviderFactoryFn = Arc::new(move |_| {
            Err(MemoryBundleError::Factory(MemoryProviderError {
                name: "Boom".into(),
                code: Some("E_BOOM".into()),
                message: thrown.clone(),
                requires_host: true,
            }))
        });
        let source = MemoryBundleFamilySource::default().with_provider_factory(factory);

        let error = stage_memory(&source, &group).expect_err("a throwing factory must fail");
        assert_eq!(
            error.message,
            String::from_utf16_lossy(message.units()),
            "the RuntimeError display message keeps the original text without a staging prefix"
        );
        let js = error
            .js_error()
            .expect("the stage boundary must retain the original JS error");
        assert_eq!(js.name, "Boom");
        assert_eq!(js.code.as_deref(), Some("E_BOOM"));
        assert_eq!(js.message.units(), message.units());
        assert!(!js.message.units().contains(&0xFFFD));
        assert!(js.requires_host);

        let projected = crate::runtime::memory::MemoryProjectionError::from(error);
        assert_eq!(projected.name(), "Boom");
        assert_eq!(projected.code(), Some("E_BOOM"));
        assert_eq!(projected.message().units(), message.units());
        assert!(projected.requires_host());

        let context = group.context();
        assert!(
            context.owner(Family::MemoryBundle).is_some(),
            "the dispose owner is registered before stage can throw"
        );
        assert!(context.resource(Resource::Memory).is_none());
        assert!(context.resource(Resource::MemoryService).is_none());
        assert!(context.resource(Resource::MemoryManagement).is_none());
        bounded(group.dispose()).await.unwrap();
    }

    #[tokio::test]
    async fn memory_provider_factory_precedes_legacy_create_provider() {
        let scratch = Scratch::new();
        let group = memory_group(&scratch.0.join("project"), memory_snapshot());
        let provider = Arc::new(FakeMemory::default());
        let legacy_calls = Arc::new(AtomicUsize::new(0));
        let provider_in_factory = Arc::clone(&provider);
        let factory: MemoryProviderFactoryFn =
            Arc::new(move |_| Ok(Some(injected_provider(&provider_in_factory))));
        let legacy_calls_in_factory = Arc::clone(&legacy_calls);
        let legacy: NativeProviderFactoryFn = Arc::new(move |_: &NativeMemoryProviderOptions| {
            legacy_calls_in_factory.fetch_add(1, Ordering::SeqCst);
            Ok(None)
        });
        let source = MemoryBundleFamilySource::default()
            .with_provider_factory(factory)
            .with_create_provider(legacy);

        stage_memory(&source, &group).expect("the provider factory must win");
        assert_eq!(
            legacy_calls.load(Ordering::SeqCst),
            0,
            "createProvider must not be consulted when providerFactory is present"
        );
        assert!(group.context().resource(Resource::Memory).is_some());
        bounded(group.dispose()).await.unwrap();
    }

    #[tokio::test]
    async fn memory_legacy_create_provider_stages_when_provider_factory_is_absent() {
        let scratch = Scratch::new();
        let group = memory_group(&scratch.0.join("project"), memory_snapshot());
        let provider = Arc::new(FakeMemory::default());
        let captured: Arc<Mutex<Option<NativeMemoryProviderOptions>>> = Arc::new(Mutex::new(None));
        let captured_in_factory = Arc::clone(&captured);
        let provider_in_factory = Arc::clone(&provider);
        let legacy: NativeProviderFactoryFn = Arc::new(move |input| {
            *captured_in_factory.lock().unwrap() = Some(input.clone());
            let memory: Arc<dyn MemoryResolver> = provider_in_factory.clone();
            let service: Arc<dyn NativeMemoryService> = provider_in_factory.clone();
            Ok(Some(NativeProjectMemoryProvider {
                provider: memory,
                service,
            }))
        });
        let source = MemoryBundleFamilySource::default().with_create_provider(legacy);

        stage_memory(&source, &group).expect("the legacy injection point must stage");
        let input = captured
            .lock()
            .unwrap()
            .clone()
            .expect("createProvider receives the stripped native options");
        assert_eq!(input.project_root, scratch.0.join("project"));
        assert_eq!(input.config, Some(expected_memory_config()));
        assert!(input.model_config.is_some());
        assert_eq!(
            input.now.as_ref().expect("the clock must be passed")(),
            UNIX_EPOCH
        );
        assert!(input.telemetry.is_none());
        assert_eq!(input.agent_model.as_deref(), Some("test/mock"));

        let management = group
            .context()
            .resource(Resource::MemoryManagement)
            .unwrap();
        let management = management
            .as_any()
            .downcast_ref::<MemoryManagementResource>()
            .unwrap();
        management.management().clear("current_project").unwrap();
        assert_eq!(provider.clear_calls.load(Ordering::SeqCst), 1);

        bounded(group.dispose()).await.unwrap();
        bounded(group.dispose()).await.unwrap();
        assert_eq!(
            provider.close_calls.load(Ordering::SeqCst),
            1,
            "the native service close runs exactly once through the provider disposer"
        );
    }

    #[tokio::test]
    async fn memory_native_default_fails_explicitly_and_does_not_publish() {
        let scratch = Scratch::new();
        let group = memory_group(&scratch.0.join("project"), memory_snapshot());
        let error = stage_memory(&MemoryBundleFamilySource::default(), &group)
            .expect_err("the unported native provider must fail explicitly");
        assert_eq!(
            error.message,
            "ProjectMemoryBundle requires an injected provider factory; the native EdgeClaw provider is not ported."
        );
        let context = group.context();
        assert!(context.owner(Family::MemoryBundle).is_some());
        assert!(context.resource(Resource::Memory).is_none());
        assert!(context.resource(Resource::MemoryService).is_none());
        assert!(context.resource(Resource::MemoryManagement).is_none());
        bounded(group.dispose()).await.unwrap();
    }

    #[tokio::test]
    async fn memory_native_default_with_disabled_config_stages_no_provider() {
        let scratch = Scratch::new();
        let mut snapshot = memory_snapshot();
        snapshot["config"]["memory"]["enabled"] = json!(false);
        let group = memory_group(&scratch.0.join("project"), snapshot);
        stage_memory(&MemoryBundleFamilySource::default(), &group)
            .expect("a disabled config stages no provider, as in the baseline");
        let context = group.context();
        assert!(context.owner(Family::MemoryBundle).is_some());
        assert!(context.resource(Resource::Memory).is_none());
        assert!(context.resource(Resource::MemoryService).is_none());
        assert!(context.resource(Resource::MemoryManagement).is_none());
        bounded(group.dispose()).await.unwrap();
    }

    #[tokio::test]
    async fn with_context_memory_wires_context_and_keeps_native_memory_default() {
        let scratch = Scratch::new();
        let group = memory_group(&scratch.0.join("project"), memory_snapshot());
        let families = Families::unwired().with_context_memory();

        families
            .context_storage
            .stage(&group.context(), &mut group.sink())
            .expect("the builder must wire the native context storage source");
        assert!(
            group
                .context()
                .resource(Resource::InstructionStorage)
                .is_some()
        );
        assert!(
            group
                .context()
                .resource(Resource::ToolResultSpill)
                .is_some()
        );

        let error = families
            .memory
            .stage(&group.context(), &mut group.sink())
            .expect_err("the builder keeps the unported native memory default");
        assert_eq!(
            error.message,
            "ProjectMemoryBundle requires an injected provider factory; the native EdgeClaw provider is not ported."
        );
        assert!(group.context().resource(Resource::Memory).is_none());
        bounded(group.dispose()).await.unwrap();
    }

    fn memory_registry(
        scratch: &Scratch,
        source: MemoryBundleFamilySource,
        fail: Arc<AtomicBool>,
        factory_calls: Arc<AtomicUsize>,
        provider: Arc<FakeMemory>,
    ) -> ProjectRuntimeRegistry {
        let factory: MemoryProviderFactoryFn = Arc::new(move |_| {
            factory_calls.fetch_add(1, Ordering::SeqCst);
            if fail.load(Ordering::SeqCst) {
                Err(MemoryBundleError::Factory(MemoryProviderError {
                    name: "DialogGatewayError".into(),
                    code: Some("MEMORY_FACTORY_FAILED".into()),
                    message: JsString::from_utf16(
                        "memory factory exploded "
                            .encode_utf16()
                            .chain([0xD800])
                            .collect::<Vec<_>>(),
                    ),
                    requires_host: true,
                }))
            } else {
                Ok(Some(injected_provider(&provider)))
            }
        });
        let families = silent_families()
            .with_context_memory()
            .with_memory(source.with_provider_factory(factory));
        registry_for_yaml(scratch, MEMORY_YAML, families)
    }

    fn assert_memory_factory_error(error: &RuntimeError) {
        let js = error
            .js_error()
            .expect("registry.resolve must retain the factory's JS error");
        assert_eq!(js.name, "DialogGatewayError");
        assert_eq!(js.code.as_deref(), Some("MEMORY_FACTORY_FAILED"));
        assert_eq!(
            js.message.units(),
            "memory factory exploded "
                .encode_utf16()
                .chain([0xD800])
                .collect::<Vec<_>>()
                .as_slice()
        );
        assert!(!js.message.units().contains(&0xFFFD));
        assert!(js.requires_host);
    }

    /// Memory is the last staged family in
    /// `ProjectRuntimeResourcesBundle.ts:387-398` and in
    /// `ProjectResourcesFactory::stage` (`families.memory.stage(...)?; Ok(())`).
    /// There is no family after memory, so a "provider created, then a later
    /// family fails" interleaving cannot exist. The reachable cases are: (a) the
    /// factory throws before a provider exists, which must leave
    /// `dispose_calls == 0`, and (b) a complete generation, whose provider is
    /// disposed exactly once. Do not invent a post-memory family to cover (a)
    /// differently.
    #[tokio::test]
    async fn memory_registry_stages_injected_provider_and_does_not_publish_failures() {
        let scratch = Scratch::new();
        let provider = Arc::new(FakeMemory::default());
        let fail = Arc::new(AtomicBool::new(true));
        let factory_calls = Arc::new(AtomicUsize::new(0));
        let registry = memory_registry(
            &scratch,
            MemoryBundleFamilySource::default(),
            Arc::clone(&fail),
            Arc::clone(&factory_calls),
            Arc::clone(&provider),
        );
        let root = scratch.0.join("project");

        let error = match registry.resolve(Some(&root)) {
            Err(error) => error,
            Ok(_) => panic!("a failing memory factory must fail the generation"),
        };
        assert_eq!(
            error.message,
            String::from_utf16_lossy(
                "memory factory exploded "
                    .encode_utf16()
                    .chain([0xD800])
                    .collect::<Vec<_>>()
                    .as_slice()
            )
        );
        assert_memory_factory_error(&error);
        let error = match registry.resolve(Some(&root)) {
            Err(error) => error,
            Ok(_) => panic!("a failed generation is never published or cached"),
        };
        assert_eq!(
            error.message,
            String::from_utf16_lossy(
                "memory factory exploded "
                    .encode_utf16()
                    .chain([0xD800])
                    .collect::<Vec<_>>()
                    .as_slice()
            )
        );
        assert_memory_factory_error(&error);
        assert_eq!(
            factory_calls.load(Ordering::SeqCst),
            2,
            "the second resolve must stage a fresh generation"
        );
        assert_eq!(
            provider.dispose_calls.load(Ordering::SeqCst),
            0,
            "the factory failed before creating a provider, so there is nothing to dispose"
        );

        fail.store(false, Ordering::SeqCst);
        let runtime = registry
            .resolve(Some(&root))
            .expect("a later complete generation must publish");
        let lease = runtime
            .acquire()
            .expect("the published generation is active");
        let context = lease
            .runtime()
            .resources()
            .get::<ProjectResources>()
            .expect("Factory resources should be published")
            .context();
        assert!(context.resource(Resource::Memory).is_some());
        assert!(context.resource(Resource::MemoryService).is_some());
        assert!(context.resource(Resource::MemoryManagement).is_some());
        assert_eq!(factory_calls.load(Ordering::SeqCst), 3);
        drop(lease);
        bounded(registry.dispose()).await.unwrap();
        bounded(registry.dispose()).await.unwrap();
        assert_eq!(
            provider.dispose_calls.load(Ordering::SeqCst),
            1,
            "registry disposal must clean the provider exactly once"
        );
    }

    fn yaml_with_memory(memory: &str) -> String {
        format!("{}memory:\n{}", CONFIG_YAML_NO_MEMORY, memory)
    }

    #[tokio::test]
    async fn memory_default_without_memory_section_stages_no_provider() {
        let scratch = Scratch::new();
        let registry = registry_for_yaml(
            &scratch,
            CONFIG_YAML_NO_MEMORY,
            silent_families().with_context_memory(),
        );
        let root = scratch.0.join("project");
        let runtime = registry
            .resolve(Some(&root))
            .expect("an absent memory section is the disabled default");
        let lease = runtime
            .acquire()
            .expect("the published generation is active");
        let context = lease
            .runtime()
            .resources()
            .get::<ProjectResources>()
            .expect("Factory resources should be published")
            .context();
        assert!(context.owner(Family::MemoryBundle).is_some());
        assert!(context.resource(Resource::Memory).is_none());
        assert!(context.resource(Resource::MemoryService).is_none());
        assert!(context.resource(Resource::MemoryManagement).is_none());
        drop(lease);
        bounded(registry.dispose()).await.unwrap();
    }

    #[tokio::test]
    async fn memory_default_with_only_provider_normalizes_enabled_and_fails_native() {
        let scratch = Scratch::new();
        let yaml = yaml_with_memory("  provider: edgeclaw\n");
        let registry = registry_for_yaml(&scratch, &yaml, silent_families().with_context_memory());
        let root = scratch.0.join("project");
        let error = match registry.resolve(Some(&root)) {
            Err(error) => error,
            Ok(_) => panic!("normalized enabled=true must reach the unported native factory"),
        };
        assert_eq!(
            error.message,
            "ProjectMemoryBundle requires an injected provider factory; the native EdgeClaw provider is not ported.",
            "a provider-only memory section normalizes enabled to true"
        );
        bounded(registry.dispose()).await.unwrap();
    }

    #[tokio::test]
    async fn memory_empty_section_defaults_enabled_and_fails_native() {
        let scratch = Scratch::new();
        let yaml = yaml_with_memory("  {}\n");
        let registry = registry_for_yaml(&scratch, &yaml, silent_families().with_context_memory());
        let error = match registry.resolve(Some(&scratch.0.join("project"))) {
            Err(error) => error,
            Ok(_) => panic!("a present-but-empty memory section must not be treated as absent"),
        };
        assert_eq!(
            error.message,
            "ProjectMemoryBundle requires an injected provider factory; the native EdgeClaw provider is not ported.",
            "`memory: {{}}` defaults enabled=true and provider=edgeclaw, unlike a missing section"
        );
        bounded(registry.dispose()).await.unwrap();
    }

    #[tokio::test]
    async fn memory_invalid_types_are_rejected_during_normalization() {
        for (memory, expected) in [
            ("  enabled: \"yes\"\n", "memory.enabled must be a boolean."),
            (
                "  provider: 5\n",
                "memory.provider must be a non-empty string.",
            ),
        ] {
            let scratch = Scratch::new();
            let yaml = yaml_with_memory(memory);
            let registry =
                registry_for_yaml(&scratch, &yaml, silent_families().with_context_memory());
            let error = match registry.resolve(Some(&scratch.0.join("project"))) {
                Err(error) => error,
                Ok(_) => panic!("invalid memory types must fail normalization: {memory}"),
            };
            assert_eq!(error.message, expected, "{memory}");
            bounded(registry.dispose()).await.unwrap();
        }
    }
}

fn event_resource(context: &StageContext) -> super::super::interaction_events::InteractionEvents {
    context
        .resource(Resource::InteractionEvents)
        .unwrap()
        .as_any()
        .downcast_ref::<super::super::interaction_events::InteractionEvents>()
        .unwrap()
        .clone()
}
fn host_event(session: &str, turn: Option<&str>, kind: &str) -> JsObject {
    let mut event = pilotdeck_protocol::JsObject::new();
    event.insert("type".into(), kind.into());
    event.insert("sessionId".into(), session.into());
    if let Some(turn) = turn {
        event.insert("turnId".into(), turn.into());
    }
    event
}
fn event_call(event: JsObject) -> JsValue {
    let mut payload = JsObject::new();
    payload.insert("operation".into(), "emit".into());
    payload.insert("event".into(), JsValue::Object(event));
    let mut call = JsObject::new();
    call.insert("payload".into(), JsValue::Object(payload));
    JsValue::Object(call)
}

#[tokio::test]
async fn interaction_event_factory_binds_real_buffer_in_both_builder_orders() {
    use crate::host::modules::{ModuleRouter, event::TurnIdentity};
    for families in [
        Families::unwired().with_interaction().with_tools_mcp(),
        Families::unwired().with_tools_mcp().with_interaction(),
    ] {
        let group = interaction_test_group();
        let context = group.context();
        families
            .execution_world
            .stage(&context, &mut group.sink())
            .unwrap();
        let events = event_resource(&context);
        let steer = steer_resource(&context);
        let steer_turn = bind_steer(&steer);
        let mut router = ModuleRouter::default();
        let port = events
            .bind_turn(
                TurnIdentity::new("session", "turn"),
                "run".into(),
                &mut router,
            )
            .unwrap();
        assert!(router.event.is_installed());
        assert!(!router.permission.is_installed());
        assert!(!router.lifecycle.is_installed());
        let mut first = host_event("session", Some("turn"), "text_delta");
        first.insert("text".into(), "中文".into());
        let second = host_event("session", None, "");
        router.event.call(event_call(first.clone())).await.unwrap();
        router.event.call(event_call(second.clone())).await.unwrap();
        assert_eq!(port.run_id(), JsString::from("run"));
        assert_eq!(router.event.drain_checked().unwrap(), vec![first, second]);
        assert!(port.drain().unwrap().is_empty());
        // Re-execute the exact baseline trace and compare against actual TS output.
        let golden: Value = serde_json::from_str(include_str!(
            "../../../tests/fixtures/interaction-events/oracle.json"
        ))
        .unwrap();
        let rows = golden.as_array().unwrap();
        let first_batch = rows[0].as_array().unwrap();
        for event in first_batch {
            let event = pilotdeck_protocol::parse_json(&event.to_string())
                .unwrap()
                .as_object()
                .unwrap()
                .clone();
            router.event.call(event_call(event)).await.unwrap();
        }
        let expected: Vec<_> = first_batch
            .iter()
            .map(|v| {
                pilotdeck_protocol::parse_json(&v.to_string())
                    .unwrap()
                    .as_object()
                    .unwrap()
                    .clone()
            })
            .collect();
        assert_eq!(port.drain().unwrap(), expected);
        assert!(rows[1].as_array().unwrap().is_empty());
        assert!(port.drain().unwrap().is_empty());
        group.dispose().await.unwrap();
        group.dispose().await.unwrap();
        assert!(events.is_closed());
        assert!(steer.is_closed());
        assert_eq!(steer.release_count(), 1);
        assert_eq!(steer_turn.release_count(), 1);
        assert_eq!(events.release_count(), 1);
        assert!(
            router
                .event
                .call(event_call(host_event("session", None, "late")))
                .await
                .is_err()
        );
        assert!(router.event.drain_checked().is_err());
        assert!(port.drain().is_err());
        assert!(
            events
                .bind_turn(
                    TurnIdentity::new("session", "next"),
                    "next".into(),
                    &mut router
                )
                .is_err()
        );
    }
}

#[tokio::test]
async fn interaction_event_runs_revoke_exact_handles_and_reject_bad_identity() {
    use crate::host::modules::{ModuleRouter, event::TurnIdentity};
    let group = interaction_test_group();
    let context = group.context();
    Families::unwired()
        .with_tools_mcp()
        .with_interaction()
        .execution_world
        .stage(&context, &mut group.sink())
        .unwrap();
    let events = event_resource(&context);
    let mut first = ModuleRouter::default();
    let old = events
        .bind_turn(
            TurnIdentity::new("session", "turn"),
            "same-run".into(),
            &mut first,
        )
        .unwrap();
    let mut other = ModuleRouter::default();
    let independent = events
        .bind_turn(
            TurnIdentity::new("other", "turn"),
            "same-run".into(),
            &mut other,
        )
        .unwrap();
    first
        .event
        .call(event_call(host_event("session", None, "buffered")))
        .await
        .unwrap();
    // Retain a handler clone across replacement; it must not reach the new run.
    let before = first.event.clone();
    let mut next = ModuleRouter::default();
    let current = events
        .bind_turn(
            TurnIdentity::new("session", "turn"),
            "same-run".into(),
            &mut next,
        )
        .unwrap();
    assert!(
        before
            .call(event_call(host_event("session", None, "late")))
            .await
            .is_err()
    );
    assert!(old.drain().is_err());
    old.close(); // cannot revoke the replacement token
    next.event
        .call(event_call(host_event("session", Some("turn"), "ok")))
        .await
        .unwrap();
    assert!(
        next.event
            .call(event_call(host_event(
                "other",
                Some("turn"),
                "wrong-session"
            )))
            .await
            .is_err()
    );
    assert!(
        next.event
            .call(event_call(host_event(
                "session",
                Some("old-turn"),
                "wrong-turn"
            )))
            .await
            .is_err()
    );
    assert_eq!(current.drain().unwrap().len(), 1);
    other
        .event
        .call(event_call(host_event("other", None, "ok")))
        .await
        .unwrap();
    assert_eq!(independent.drain().unwrap().len(), 1);
    current.close();
    assert!(
        next.event
            .call(event_call(host_event("session", None, "closed")))
            .await
            .is_err()
    );
    group.dispose().await.unwrap();
}

#[tokio::test]
async fn interaction_event_registry_keeps_old_generation_until_lease_drain() {
    use crate::host::modules::{ModuleRouter, event::TurnIdentity};
    let scratch = Scratch::new();
    let probe = Arc::new(Probe::default());
    let registry = interaction_fake_registry(&scratch, &probe);
    let root = scratch.0.join("a");
    let runtime = registry.resolve(Some(&root)).unwrap();
    let mut lease = runtime.acquire().unwrap();
    let old_context = lease
        .runtime()
        .resources()
        .get::<ProjectResources>()
        .unwrap()
        .context();
    let old = event_resource(&old_context);
    let old_steer = steer_resource(&old_context);
    let old_steer_turn = bind_steer(&old_steer);
    let mut old_router = ModuleRouter::default();
    old.bind_turn(
        TurnIdentity::new("session", "turn"),
        "run".into(),
        &mut old_router,
    )
    .unwrap();
    bounded(registry.reload()).await.unwrap();
    let current = registry.resolve(Some(&root)).unwrap();
    let current_lease = current.acquire().unwrap();
    let new_context = current_lease
        .runtime()
        .resources()
        .get::<ProjectResources>()
        .unwrap()
        .context();
    let new = event_resource(&new_context);
    let new_steer = steer_resource(&new_context);
    let new_steer_turn = bind_steer(&new_steer);
    assert!(!old_steer.is_closed());
    assert_eq!(old_steer_turn.release_count(), 0);
    assert!(!new_steer.is_closed());
    let mut new_router = ModuleRouter::default();
    new.bind_turn(
        TurnIdentity::new("session", "turn"),
        "run".into(),
        &mut new_router,
    )
    .unwrap();
    old_router
        .event
        .call(event_call(host_event("session", None, "old-live")))
        .await
        .unwrap();
    assert!(new_router.event.drain_checked().unwrap().is_empty());
    lease.release().await.unwrap();
    assert!(old.is_closed());
    assert!(old_steer.is_closed());
    assert_eq!(old_steer.release_count(), 1);
    assert_eq!(old_steer_turn.release_count(), 1);
    assert!(!new_steer.is_closed());
    assert_eq!(new_steer_turn.release_count(), 0);
    assert_eq!(old.release_count(), 1);
    assert!(
        old_router
            .event
            .call(event_call(host_event("session", None, "old-closed")))
            .await
            .is_err()
    );
    assert!(old_router.event.drain_checked().is_err());
    new_router
        .event
        .call(event_call(host_event("session", None, "new-live")))
        .await
        .unwrap();
    assert_eq!(new_router.event.drain_checked().unwrap().len(), 1);
    drop(current_lease);
    bounded(registry.dispose()).await.unwrap();
    assert_eq!(new.release_count(), 1);
    assert!(new_steer.is_closed());
    assert_eq!(new_steer.release_count(), 1);
    assert_eq!(new_steer_turn.release_count(), 1);
}

fn permission_resource(
    context: &StageContext,
) -> super::super::interaction_permission::InteractionPermission {
    context
        .resource(Resource::InteractionPermission)
        .unwrap()
        .as_any()
        .downcast_ref::<super::super::interaction_permission::InteractionPermission>()
        .unwrap()
        .clone()
}
fn bind_permission(
    resource: &super::super::interaction_permission::InteractionPermission,
) -> super::super::interaction_permission::TurnPermission {
    use crate::runtime::{permission, project::interaction_permission::PermissionTurnOptions};
    struct EmptyCatalog;
    impl crate::host::modules::capability::ToolCatalog for EmptyCatalog {
        fn list(
            &self,
        ) -> crate::host::HostFuture<Vec<Arc<dyn crate::host::modules::capability::ToolDefinition>>>
        {
            Box::pin(async { Ok(vec![]) })
        }
    }
    let mut router = crate::host::modules::ModuleRouter::default();
    assert!(!router.permission.is_installed());
    let turn = resource
        .bind_turn(
            "session".into(),
            "run".into(),
            PermissionTurnOptions {
                profile: RuntimeProfile::resolve(
                    &JsValue::Object(JsObject::new()),
                    &RuntimeProfileOverrides::default(),
                )
                .interaction,
                context: permission::create_default_permission_context(
                    permission::DefaultPermissionContextOptions {
                        cwd: "/workspace".into(),
                        ..Default::default()
                    },
                ),
                rules: crate::config::permission::RuleSetHandle::empty(),
                coordinator: Arc::default(),
                emit: Arc::new(|_| false),
                timeout: std::time::Duration::from_secs(1),
            },
            Arc::new(EmptyCatalog),
            &mut router,
        )
        .unwrap();
    assert!(router.permission.is_installed());
    turn
}
#[tokio::test]
async fn interaction_permission_factory_binds_and_closes_in_both_builder_orders() {
    use crate::host::modules::permission::PermissionContext as _;
    for families in [
        Families::unwired().with_interaction().with_tools_mcp(),
        Families::unwired().with_tools_mcp().with_interaction(),
    ] {
        let group = interaction_test_group();
        let context = group.context();
        families
            .execution_world
            .stage(&context, &mut group.sink())
            .unwrap();
        let resource = permission_resource(&context);
        let steer = steer_resource(&context);
        let steer_turn = bind_steer(&steer);
        let turn = bind_permission(&resource);
        assert!(turn.prepare(JsValue::Null).await.is_ok());
        group.dispose().await.unwrap();
        group.dispose().await.unwrap();
        assert!(resource.is_closed());
        assert!(steer.is_closed());
        assert_eq!(steer.release_count(), 1);
        assert_eq!(steer_turn.release_count(), 1);
        assert_eq!(resource.release_count(), 1);
        assert!(turn.prepare(JsValue::Null).await.is_err());
    }
}
#[tokio::test]
async fn interaction_permission_registry_keeps_old_generation_until_lease_drain() {
    use crate::host::modules::permission::PermissionContext as _;
    let scratch = Scratch::new();
    let probe = Arc::new(Probe::default());
    let registry = interaction_fake_registry(&scratch, &probe);
    let root = scratch.0.join("a");
    let runtime = registry.resolve(Some(&root)).unwrap();
    let mut lease = runtime.acquire().unwrap();
    let old_context = lease
        .runtime()
        .resources()
        .get::<ProjectResources>()
        .unwrap()
        .context();
    let old = permission_resource(&old_context);
    let old_steer = steer_resource(&old_context);
    let old_steer_turn = bind_steer(&old_steer);
    let old_turn = bind_permission(&old);
    bounded(registry.reload()).await.unwrap();
    let current = registry.resolve(Some(&root)).unwrap();
    let current_lease = current.acquire().unwrap();
    let new_context = current_lease
        .runtime()
        .resources()
        .get::<ProjectResources>()
        .unwrap()
        .context();
    let new = permission_resource(&new_context);
    let new_steer = steer_resource(&new_context);
    let new_steer_turn = bind_steer(&new_steer);
    assert!(!old_steer.is_closed());
    assert_eq!(old_steer_turn.release_count(), 0);
    assert!(!new_steer.is_closed());
    let new_turn = bind_permission(&new);
    assert!(old_turn.prepare(JsValue::Null).await.is_ok());
    lease.release().await.unwrap();
    assert!(old.is_closed());
    assert!(old_steer.is_closed());
    assert_eq!(old_steer.release_count(), 1);
    assert_eq!(old_steer_turn.release_count(), 1);
    assert!(!new_steer.is_closed());
    assert_eq!(new_steer_turn.release_count(), 0);
    assert_eq!(old.release_count(), 1);
    assert!(old_turn.prepare(JsValue::Null).await.is_err());
    assert!(new_turn.prepare(JsValue::Null).await.is_ok());
    drop(current_lease);
    bounded(registry.dispose()).await.unwrap();
    assert!(new.is_closed());
    assert_eq!(new.release_count(), 1);
    assert!(new_steer.is_closed());
    assert_eq!(new_steer.release_count(), 1);
    assert_eq!(new_steer_turn.release_count(), 1);
}

fn hooks_resource(context: &StageContext) -> super::super::interaction_hooks::InteractionHooks {
    context
        .resource(Resource::InteractionHooks)
        .unwrap()
        .as_any()
        .downcast_ref::<super::super::interaction_hooks::InteractionHooks>()
        .unwrap()
        .clone()
}
fn bind_hooks(
    resource: &super::super::interaction_hooks::InteractionHooks,
) -> (
    super::super::interaction_hooks::TurnHooks,
    crate::host::modules::ModuleRouter,
) {
    use crate::host::modules::{ModuleRouter, lifecycle::Binding};
    let mut router = ModuleRouter::default();
    let turn = resource
        .bind_turn(
            Binding {
                session_id: "session".into(),
                turn_id: "turn".into(),
                cwd: "/workspace".into(),
                permission_mode: None,
                base_env: Some(JsObject::new()),
                signal: None,
            },
            "run".into(),
            vec![],
            None,
            &mut router,
        )
        .unwrap();
    (turn, router)
}
#[tokio::test]
async fn interaction_hooks_factory_binds_and_rolls_back_in_both_builder_orders() {
    for families in [
        Families::unwired().with_interaction().with_tools_mcp(),
        Families::unwired().with_tools_mcp().with_interaction(),
    ] {
        let group = interaction_test_group();
        let context = group.context();
        families
            .execution_world
            .stage(&context, &mut group.sink())
            .unwrap();
        let resource = hooks_resource(&context);
        let (turn, router) = bind_hooks(&resource);
        assert!(router.lifecycle.is_installed());
        assert!(turn.pending_receipts().unwrap().is_empty());
        group.dispose().await.unwrap();
        group.dispose().await.unwrap();
        assert!(resource.is_closed());
        assert_eq!(resource.release_count(), 1);
        assert_eq!(turn.release_count(), 1);
        assert!(turn.pending_receipts().is_err());
    }
    // A later family failure invokes the same concrete resource cleanup.
    use super::super::interaction_hooks::{InteractionHooks, TurnHooks};
    type Captured = Arc<
        Mutex<
            Option<(
                InteractionHooks,
                TurnHooks,
                super::super::interaction_steer::InteractionSteer,
                super::super::interaction_steer::TurnSteer,
            )>,
        >,
    >;
    struct FailAfterHooks(Captured);
    impl ResourceFamilySource for FailAfterHooks {
        fn family(&self) -> Family {
            Family::ContextStorage
        }
        fn stage(&self, context: &StageContext, _: &mut StageSink<'_>) -> RuntimeResult {
            let hooks = hooks_resource(context);
            let (turn, _) = bind_hooks(&hooks);
            let steer = steer_resource(context);
            let steer_turn = bind_steer(&steer);
            *self.0.lock().unwrap() = Some((hooks, turn, steer, steer_turn));
            Err(RuntimeError::new("injected after hooks"))
        }
    }
    let scratch = Scratch::new();
    let probe = Arc::new(Probe::default());
    std::fs::create_dir_all(scratch.0.join(".pilotdeck")).unwrap();
    std::fs::write(
        scratch.0.join(".pilotdeck/pilotdeck.yaml"),
        PROJECT_CONFIGS[0].1,
    )
    .unwrap();
    std::fs::create_dir_all(scratch.0.join("config-home")).unwrap();
    std::fs::write(scratch.0.join("config-home/pilotdeck.yaml"), GLOBAL_YAML).unwrap();
    let captured: Captured = Arc::default();
    let mut families = families(&probe).with_interaction();
    families.context_storage = Arc::new(FailAfterHooks(captured.clone()));
    let mut env = crate::config::Env::new();
    env.insert(
        "PILOT_HOME".into(),
        scratch.0.join("config-home").to_string_lossy().into_owned(),
    );
    let factory = ProjectResourcesFactory::new(ProjectResourcesOptions {
        pilot_home: scratch.0.join("home"),
        cwd: scratch.0.clone(),
        env,
        homedir: Some(scratch.0.join("home")),
        builtin_skills_root: None,
        now: Arc::new(|| SystemTime::UNIX_EPOCH),
        config_version: None,
        runtime_profile_overrides: Default::default(),
        telemetry: None,
        families,
    });
    let registry = ProjectRuntimeRegistry::new(scratch.0.clone(), Arc::new(factory));
    let error = registry
        .resolve(Some(&scratch.0))
        .err()
        .expect("injected stage error");
    assert_eq!(error, RuntimeError::new("injected after hooks"));
    let (resource, turn, steer, steer_turn) = captured.lock().unwrap().take().unwrap();
    until(|| resource.is_closed()).await;
    bounded(turn.wait_closed()).await.unwrap();
    assert_eq!(resource.release_count(), 1);
    assert_eq!(turn.release_count(), 1);
    assert!(turn.pending_receipts().is_err());
    bounded(registry.dispose()).await.unwrap();
    bounded(steer_turn.close()).await.unwrap();
    assert!(steer.is_closed());
    assert_eq!(steer.release_count(), 1);
    assert_eq!(steer_turn.release_count(), 1);
}
#[tokio::test]
async fn interaction_hooks_registry_keeps_old_generation_until_lease_drain() {
    let scratch = Scratch::new();
    let probe = Arc::new(Probe::default());
    let registry = interaction_fake_registry(&scratch, &probe);
    let root = scratch.0.join("a");
    let runtime = registry.resolve(Some(&root)).unwrap();
    let mut lease = runtime.acquire().unwrap();
    let old = hooks_resource(
        &lease
            .runtime()
            .resources()
            .get::<ProjectResources>()
            .unwrap()
            .context(),
    );
    let (old_turn, _) = bind_hooks(&old);
    let steer = steer_resource(
        &lease
            .runtime()
            .resources()
            .get::<ProjectResources>()
            .unwrap()
            .context(),
    );
    let steer_turn = bind_steer(&steer);
    bounded(registry.reload()).await.unwrap();
    let current = registry.resolve(Some(&root)).unwrap();
    let current_lease = current.acquire().unwrap();
    let new = hooks_resource(
        &current_lease
            .runtime()
            .resources()
            .get::<ProjectResources>()
            .unwrap()
            .context(),
    );
    let (new_turn, _) = bind_hooks(&new);
    assert!(old_turn.pending_receipts().is_ok());
    assert!(!steer.is_closed());
    lease.release().await.unwrap();
    assert!(old.is_closed());
    assert!(steer.is_closed());
    assert_eq!(steer_turn.release_count(), 1);
    assert!(steer.bind_session("late".into(), None).is_err());
    assert_eq!(old.release_count(), 1);
    assert_eq!(old_turn.release_count(), 1);
    assert!(old_turn.pending_receipts().is_err());
    assert!(new_turn.pending_receipts().is_ok());
    drop(current_lease);
    bounded(registry.dispose()).await.unwrap();
    assert!(new.is_closed());
    assert_eq!(new.release_count(), 1);
    assert_eq!(new_turn.release_count(), 1);
}

fn steer_resource(context: &StageContext) -> super::super::interaction_steer::InteractionSteer {
    context
        .resource(Resource::InteractionSteer)
        .unwrap()
        .as_any()
        .downcast_ref::<super::super::interaction_steer::InteractionSteer>()
        .unwrap()
        .clone()
}
fn bind_steer(
    resource: &super::super::interaction_steer::InteractionSteer,
) -> super::super::interaction_steer::TurnSteer {
    resource
        .bind_session("s".into(), None)
        .unwrap()
        .start_turn(
            "t".into(),
            "same".into(),
            vec![],
            crate::runtime::hooks::Cancellation::new(),
            &mut crate::host::modules::ModuleRouter::default(),
        )
        .unwrap()
}

#[path = "interaction_host_tests.rs"]
mod interaction_host;

#[path = "factory_memory_tests.rs"]
mod context_memory_tests;
