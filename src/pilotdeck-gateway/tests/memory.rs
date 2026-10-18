//! Replay of the real-TS memory fixture: ProjectMemoryBundle stage/dispose and
//! the createLocalGateway memoryList/memoryWipe projection.
#[path = "common/baseline.rs"]
mod baseline;

use pilotdeck_gateway::runtime::{
    memory::{
        MemoryBundleError, MemoryConfig, MemoryDisposeFn, MemoryFuture, MemoryListInput,
        MemoryMaintenanceService, MemoryManagementService, MemoryProjection, MemoryProviderError,
        MemoryResolver, MemoryWipeInput, NativeMemoryProviderOptions, NativeMemoryService,
        NativeProjectMemoryProvider, ProjectMemoryBundle, ProjectMemoryBundleOptions,
        ProjectMemoryProvider, ProjectMemoryRegistry, ProjectMemoryResources,
    },
    project::{ProjectRuntimeRegistry, ResourceBundle, RuntimeError, RuntimeResult},
};
use pilotdeck_protocol::{JsObject, JsValue, parse_json};
use serde_json::{Value, json};
use std::{
    future::Future,
    path::{Path, PathBuf},
    pin::Pin,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
};

const ORACLE: &str = include_str!("fixtures/memory/oracle.json");
const PROJECT: &str = "/pdrs-fixture/project";

fn js(value: &Value) -> JsValue {
    parse_json(&value.to_string()).expect("fixture JSON")
}
fn error_js(value: &Value) -> JsValue {
    let mut object = JsObject::new();
    object.insert(
        "name".into(),
        JsValue::from(value["name"].as_str().unwrap()),
    );
    object.insert(
        "code".into(),
        value["code"].as_str().map_or(JsValue::Null, JsValue::from),
    );
    object.insert(
        "message".into(),
        JsValue::from(value["message"].as_str().unwrap()),
    );
    JsValue::Object(object)
}

#[derive(Default)]
struct Events(Mutex<Vec<String>>);
impl Events {
    fn push(&self, event: &str) {
        self.0.lock().unwrap().push(event.to_owned());
    }
    fn snapshot(&self) -> Vec<String> {
        self.0.lock().unwrap().clone()
    }
}

struct FakeResolver;
impl MemoryResolver for FakeResolver {}
struct FakeMaintenance;
impl MemoryMaintenanceService for FakeMaintenance {
    fn run_due_scheduled_maintenance(&self, _: &str) -> MemoryFuture<'_, ()> {
        Box::pin(async { Ok(()) })
    }
}
struct FakeManagement;
impl MemoryManagementService for FakeManagement {
    fn list(&self, _: usize) -> Result<Vec<JsValue>, MemoryProviderError> {
        Ok(Vec::new())
    }
    fn clear(&self, _: &str) -> Result<(), MemoryProviderError> {
        Ok(())
    }
    fn clear_session(&self, _: &str) -> Result<(), MemoryProviderError> {
        Ok(())
    }
}

struct FakeNativeService {
    events: Arc<Events>,
}
impl NativeMemoryService for FakeNativeService {
    fn run_due_scheduled_maintenance(&self, _: &str) -> MemoryFuture<'_, ()> {
        Box::pin(async { Ok(()) })
    }
    fn list(&self, _: usize) -> Result<Vec<JsValue>, MemoryProviderError> {
        Ok(Vec::new())
    }
    fn clear(&self, _: &str) -> Result<(), MemoryProviderError> {
        Ok(())
    }
    fn clear_session(&self, _: &str) -> Result<(), MemoryProviderError> {
        Ok(())
    }
    fn close(&self) {
        self.events.push("close");
    }
}

fn resources_json(resources: &ProjectMemoryResources) -> Value {
    json!({
        "keys": resources.js_keys(),
        "memory": resources.memory.is_some(),
        "maintenance": resources.maintenance.is_some(),
        "management": resources.management.is_some(),
    })
}

fn bundle_error_json(error: &MemoryBundleError) -> Value {
    json!({
        "name": "Error",
        "code": Value::Null,
        "message": error.to_string(),
    })
}

fn runtime_error_json(error: &RuntimeError) -> Value {
    json!({
        "name": "Error",
        "code": Value::Null,
        "message": error.message,
    })
}

fn assert_dispose_outcome(result: &RuntimeResult, step: &Value, label: &str) {
    match (result, step.get("disposeError")) {
        (Ok(()), None) => {}
        (Err(error), Some(expected)) => {
            assert_eq!(&runtime_error_json(error), expected, "{label}");
        }
        (Err(error), None) => panic!("{label}: unexpected dispose error: {error}"),
        (Ok(()), Some(expected)) => {
            panic!("{label}: expected dispose error {expected}, got success")
        }
    }
}

fn options_from_fixture(case: &Value) -> ProjectMemoryBundleOptions {
    let options = &case["options"];
    let project_root = options["projectRoot"].as_str().unwrap();
    let mut bundle = ProjectMemoryBundleOptions::new(project_root);
    if let Some(config) = options.get("config").filter(|value| !value.is_null()) {
        bundle.config = Some(MemoryConfig {
            enabled: config["enabled"].as_bool().unwrap(),
            provider: config["provider"].as_str().unwrap().to_owned(),
        });
    }
    bundle.agent_model = options["agentModel"].as_str().map(str::to_owned);
    bundle
}

fn options_json(options: &ProjectMemoryBundleOptions) -> Value {
    json!({
        "projectRoot": options.project_root.to_string_lossy(),
        "agentModel": options.agent_model,
        "hasProviderFactory": options.provider_factory.is_some(),
        "hasCreateProvider": options.create_provider.is_some(),
        "config": options.config.as_ref().map(|config| json!({
            "enabled": config.enabled,
            "provider": config.provider.clone(),
        })),
    })
}

fn provider_with_events(events: Arc<Events>, with_services: bool) -> ProjectMemoryProvider {
    let dispose: MemoryDisposeFn = Arc::new(move || {
        let events = Arc::clone(&events);
        Box::pin(async move {
            events.push("dispose");
            Ok(())
        })
    });
    ProjectMemoryProvider {
        memory: Arc::new(FakeResolver),
        maintenance: with_services
            .then(|| Arc::new(FakeMaintenance) as Arc<dyn MemoryMaintenanceService>),
        management: with_services
            .then(|| Arc::new(FakeManagement) as Arc<dyn MemoryManagementService>),
        dispose: Some(dispose),
    }
}

fn bundle_from_fixture(
    case: &Value,
    events: Arc<Events>,
    captured_factory: Arc<Mutex<Option<Value>>>,
    captured_native: Arc<Mutex<Option<Value>>>,
    factory_calls: Arc<AtomicUsize>,
) -> ProjectMemoryBundle {
    let mut options = options_from_fixture(case);
    match case["factory"].as_str().unwrap() {
        "success" => {
            options.provider_factory = Some(Arc::new(move |input: ProjectMemoryBundleOptions| {
                factory_calls.fetch_add(1, Ordering::SeqCst);
                *captured_factory.lock().unwrap() = Some(options_json(&input));
                events.push("factory");
                Ok(Some(provider_with_events(Arc::clone(&events), true)))
            }));
        }
        "throws" => {
            let factory_events = Arc::clone(&events);
            options.provider_factory = Some(Arc::new(move |_: ProjectMemoryBundleOptions| {
                factory_calls.fetch_add(1, Ordering::SeqCst);
                factory_events.push("factory");
                Err(MemoryBundleError::Factory("factory boom".into()))
            }));
        }
        "undefined" => {
            let factory_events = Arc::clone(&events);
            options.provider_factory = Some(Arc::new(move |_: ProjectMemoryBundleOptions| {
                factory_calls.fetch_add(1, Ordering::SeqCst);
                factory_events.push("factory");
                Ok(None)
            }));
        }
        "native" => {
            let factory_events = Arc::clone(&events);
            let factory_calls_native = Arc::clone(&factory_calls);
            options.create_provider = Some(Arc::new(move |input: &NativeMemoryProviderOptions| {
                factory_calls_native.fetch_add(1, Ordering::SeqCst);
                factory_events.push("create");
                *captured_native.lock().unwrap() = Some(json!({
                    "projectRoot": input.project_root.to_string_lossy(),
                    "agentModel": input.agent_model,
                    "hasProviderFactory": false,
                    "hasCreateProvider": false,
                    "config": input.config.as_ref().map(|config| json!({
                        "enabled": config.enabled,
                        "provider": config.provider,
                    })),
                }));
                Ok(Some(NativeProjectMemoryProvider {
                    provider: Arc::new(FakeResolver),
                    service: Arc::new(FakeNativeService {
                        events: Arc::clone(&factory_events),
                    }),
                }))
            }));
        }
        "preferred" => {
            options.provider_factory = Some(Arc::new(move |input: ProjectMemoryBundleOptions| {
                factory_calls.fetch_add(1, Ordering::SeqCst);
                *captured_factory.lock().unwrap() = Some(options_json(&input));
                events.push("factory");
                Ok(Some(provider_with_events(Arc::clone(&events), false)))
            }));
            options.create_provider = Some(Arc::new(
                |_: &NativeMemoryProviderOptions| -> Result<Option<NativeProjectMemoryProvider>, MemoryBundleError> {
                    panic!("createProvider must not run when providerFactory is present")
                },
            ));
        }
        "memory-only" => {
            options.provider_factory = Some(Arc::new(move |_: ProjectMemoryBundleOptions| {
                factory_calls.fetch_add(1, Ordering::SeqCst);
                events.push("factory");
                Ok(Some(ProjectMemoryProvider {
                    memory: Arc::new(FakeResolver),
                    maintenance: None,
                    management: None,
                    dispose: None,
                }))
            }));
        }
        "dispose-throws" => {
            let factory_events = Arc::clone(&events);
            options.provider_factory = Some(Arc::new(move |_: ProjectMemoryBundleOptions| {
                factory_calls.fetch_add(1, Ordering::SeqCst);
                factory_events.push("factory");
                let dispose_events = Arc::clone(&factory_events);
                Ok(Some(ProjectMemoryProvider {
                    memory: Arc::new(FakeResolver),
                    maintenance: None,
                    management: None,
                    dispose: Some(Arc::new(move || {
                        let events = Arc::clone(&dispose_events);
                        Box::pin(async move {
                            events.push("dispose");
                            Err(RuntimeError::new("dispose boom"))
                        })
                    })),
                }))
            }));
        }
        "none" => {}
        other => panic!("unknown factory mode {other}"),
    }
    ProjectMemoryBundle::new(options)
}

#[tokio::test]
async fn real_ts_memory_bundle_oracle() {
    baseline::oracle(ORACLE);
    let oracle: Value = serde_json::from_str(ORACLE).unwrap();
    assert_eq!(oracle["sourceSha256"].as_object().unwrap().len(), 3);
    let cases = oracle["bundleCases"].as_array().unwrap();
    assert_eq!(cases.len(), 10);
    let mut replayed = 0;
    for case in cases {
        let events = Arc::new(Events::default());
        let captured_factory = Arc::new(Mutex::new(None));
        let captured_native = Arc::new(Mutex::new(None));
        let factory_calls = Arc::new(AtomicUsize::new(0));
        let bundle = bundle_from_fixture(
            case,
            Arc::clone(&events),
            Arc::clone(&captured_factory),
            Arc::clone(&captured_native),
            Arc::clone(&factory_calls),
        );
        let mut pending: Vec<Pin<Box<dyn Future<Output = RuntimeResult> + Send + '_>>> = Vec::new();

        for step in case["steps"].as_array().unwrap() {
            match step["op"].as_str().unwrap() {
                "stage" | "stage-again" => {
                    let result = bundle.stage();
                    if let Some(expected) = step.get("result") {
                        assert_eq!(
                            resources_json(&result.expect("stage succeeds")),
                            *expected,
                            "{}:{}",
                            case["name"],
                            step["op"]
                        );
                    } else {
                        assert_eq!(
                            bundle_error_json(&result.expect_err("stage fails")),
                            step["error"],
                            "{}:{}",
                            case["name"],
                            step["op"]
                        );
                    }
                }
                "dispose" => {
                    let first = bundle.dispose();
                    let second = bundle.dispose();
                    events.push("after-dispose-call");
                    assert_eq!(
                        json!(events.snapshot()),
                        step["beforeAwaitEvents"],
                        "{}:dispose before await",
                        case["name"]
                    );
                    let first_result = first.await;
                    let second_result = second.await;
                    assert_eq!(
                        first_result, second_result,
                        "{}: repeated dispose observes the same completion",
                        case["name"]
                    );
                    assert_dispose_outcome(
                        &first_result,
                        step,
                        &format!("{}:dispose", case["name"]),
                    );
                    events.push("after-await");
                    assert_eq!(
                        json!(events.snapshot()),
                        step["afterAwaitEvents"],
                        "{}:dispose",
                        case["name"]
                    );
                }
                "dispose-call" => {
                    pending.push(bundle.dispose());
                    events.push("after-dispose-call");
                }
                "dispose-await" => {
                    // The TS case calls dispose a second time after the
                    // (failed) stage, then captures the pre-await events.
                    pending.push(bundle.dispose());
                    assert_eq!(
                        json!(events.snapshot()),
                        step["beforeAwaitEvents"],
                        "{}:dispose-await before await",
                        case["name"]
                    );
                    let mut results = Vec::new();
                    for disposal in pending.drain(..) {
                        results.push(disposal.await);
                    }
                    for result in &results {
                        assert_eq!(
                            result, &results[0],
                            "{}: every dispose caller observes the same completion",
                            case["name"]
                        );
                    }
                    assert_dispose_outcome(
                        &results[0],
                        step,
                        &format!("{}:dispose-await", case["name"]),
                    );
                    events.push("after-await");
                    assert_eq!(
                        json!(events.snapshot()),
                        step["afterAwaitEvents"],
                        "{}:dispose-await",
                        case["name"]
                    );
                }
                other => panic!("unknown bundle step {other}"),
            }
        }
        assert!(pending.is_empty(), "{}: unreplayed dispose", case["name"]);
        assert_eq!(
            json!(events.snapshot()),
            case["events"],
            "{}: events",
            case["name"]
        );
        if let Some(expected) = case.get("factoryOptions") {
            assert_eq!(
                captured_factory.lock().unwrap().as_ref().unwrap(),
                expected,
                "{}: factory options",
                case["name"]
            );
        }
        if let Some(expected) = case.get("nativeOptions") {
            assert_eq!(
                captured_native.lock().unwrap().as_ref().unwrap(),
                expected,
                "{}: native options",
                case["name"]
            );
        }
        if let Some(expected) = case.get("factoryCalls") {
            assert_eq!(
                factory_calls.load(Ordering::SeqCst),
                expected.as_u64().unwrap() as usize,
                "{}: factory calls",
                case["name"]
            );
        }
        replayed += 1;
    }
    assert_eq!(replayed, 10);
    assert!(oracle["projectionCases"].as_array().unwrap().len() == 8);
}

fn optional_path(input: &Value, key: &str) -> Option<PathBuf> {
    input.get(key).and_then(Value::as_str).map(PathBuf::from)
}
fn optional_string(input: &Value, key: &str) -> Option<String> {
    input.get(key).and_then(Value::as_str).map(str::to_owned)
}

struct ProjectionRegistry {
    calls: Arc<Mutex<Vec<Value>>>,
    management: Option<Arc<dyn MemoryManagementService>>,
    resolve_key_fail: Option<String>,
    resolve_fail: Option<String>,
}
impl ProjectMemoryRegistry for ProjectionRegistry {
    fn resolve_project_key(&self, project_key: &Path) -> Result<PathBuf, MemoryProviderError> {
        self.calls.lock().unwrap().push(json!({
            "op": "resolveProjectKey",
            "key": project_key.to_string_lossy(),
        }));
        if let Some(message) = &self.resolve_key_fail {
            return Err(MemoryProviderError::new(message.clone()));
        }
        Ok(PathBuf::from(format!("resolved:{}", project_key.display())))
    }
    fn memory_management(
        &self,
        project_key: &Path,
    ) -> Result<Option<Arc<dyn MemoryManagementService>>, MemoryProviderError> {
        self.calls.lock().unwrap().push(json!({
            "op": "resolve",
            "key": project_key.to_string_lossy(),
        }));
        if let Some(message) = &self.resolve_fail {
            return Err(MemoryProviderError::new(message.clone()));
        }
        Ok(self.management.clone())
    }
}

struct ProjectionManagement {
    calls: Arc<Mutex<Vec<Value>>>,
    items: Vec<Value>,
    fail: Option<(String, String)>,
}
impl ProjectionManagement {
    fn maybe_fail(&self, operation: &str) -> Result<(), MemoryProviderError> {
        match &self.fail {
            Some((op, message)) if op == operation => {
                Err(MemoryProviderError::new(message.clone()))
            }
            _ => Ok(()),
        }
    }
}
impl MemoryManagementService for ProjectionManagement {
    fn list(&self, limit: usize) -> Result<Vec<JsValue>, MemoryProviderError> {
        self.calls
            .lock()
            .unwrap()
            .push(json!({"op": "list", "limit": limit}));
        self.maybe_fail("list")?;
        Ok(self.items.iter().map(js).collect())
    }
    fn clear(&self, scope: &str) -> Result<(), MemoryProviderError> {
        self.calls
            .lock()
            .unwrap()
            .push(json!({"op": "clear", "scope": scope}));
        self.maybe_fail("clear")
    }
    fn clear_session(&self, session_key: &str) -> Result<(), MemoryProviderError> {
        self.calls
            .lock()
            .unwrap()
            .push(json!({"op": "clearSession", "sessionKey": session_key}));
        self.maybe_fail("clearSession")
    }
}

#[test]
fn real_ts_memory_projection_oracle() {
    let _ = baseline::oracle(ORACLE);
    let oracle: Value = serde_json::from_str(ORACLE).unwrap();
    let cases = oracle["projectionCases"].as_array().unwrap();
    assert_eq!(cases.len(), 8);
    let mut replayed = 0;
    for case in cases {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let fail = case.get("fail").map(|fail| {
            (
                fail["op"].as_str().unwrap().to_owned(),
                fail["message"].as_str().unwrap().to_owned(),
            )
        });
        let management: Option<Arc<dyn MemoryManagementService>> =
            if case["management"] == "available" {
                Some(Arc::new(ProjectionManagement {
                    calls: Arc::clone(&calls),
                    items: case["items"].as_array().unwrap().clone(),
                    fail,
                }))
            } else {
                None
            };
        let registry = Arc::new(ProjectionRegistry {
            calls: Arc::clone(&calls),
            management,
            resolve_key_fail: case["resolveProjectKeyFails"].as_str().map(str::to_owned),
            resolve_fail: case["registryResolveFails"].as_str().map(str::to_owned),
        });
        let projection = MemoryProjection::new(registry, "/pdrs-fixture/fallback");
        for step in case["steps"].as_array().unwrap() {
            calls.lock().unwrap().clear();
            let input = &step["input"];
            let actual = match step["op"].as_str().unwrap() {
                "list" => projection
                    .list(&MemoryListInput {
                        project_key: optional_path(input, "projectKey"),
                        session_key: optional_string(input, "sessionKey"),
                    })
                    .map(|result| result.to_js())
                    .map_err(|error| error.to_js()),
                "wipe" => projection
                    .wipe(&MemoryWipeInput {
                        project_key: optional_path(input, "projectKey"),
                        session_key: optional_string(input, "sessionKey"),
                        scope: optional_string(input, "scope"),
                    })
                    .map(|result| result.to_js())
                    .map_err(|error| error.to_js()),
                other => panic!("unknown projection step {other}"),
            };
            let expected: JsValue = if let Some(result) = step.get("resultJson") {
                parse_json(result.as_str().unwrap()).unwrap()
            } else {
                error_js(&step["error"])
            };
            match actual {
                Ok(actual) => {
                    assert!(
                        step.get("resultJson").is_some(),
                        "{}: unexpected success",
                        case["name"]
                    );
                    assert_eq!(actual, expected, "{}: result", case["name"]);
                }
                Err(actual) => {
                    assert!(
                        step.get("error").is_some(),
                        "{}: unexpected error",
                        case["name"]
                    );
                    assert_eq!(actual, expected, "{}: error", case["name"]);
                }
            }
            assert_eq!(
                json!(calls.lock().unwrap().clone()),
                step["calls"],
                "{}: calls",
                case["name"]
            );
        }
        replayed += 1;
    }
    assert_eq!(replayed, 8);
}

struct Scratch(PathBuf);
impl Scratch {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("pdrs-memory-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let name = self.0.file_name().unwrap().to_string_lossy();
        assert!(
            name.starts_with("pdrs-memory-"),
            "refusing to remove {}",
            self.0.display()
        );
        std::fs::remove_dir_all(&self.0).unwrap();
    }
}

#[tokio::test]
async fn memory_bundle_is_a_project_runtime_resource_group() {
    let scratch = Scratch::new();
    let root = scratch.0.clone();
    let events = Arc::new(Events::default());
    let factory = Arc::new({
        let events = Arc::clone(&events);
        move |project_root: &Path, resources: &mut ResourceBundle| {
            let mut options = ProjectMemoryBundleOptions::new(project_root.to_owned());
            options.config = Some(MemoryConfig {
                enabled: true,
                provider: "edgeclaw".into(),
            });
            options.provider_factory = Some(Arc::new({
                let events = Arc::clone(&events);
                move |_: ProjectMemoryBundleOptions| {
                    events.push("factory");
                    Ok(Some(provider_with_events(Arc::clone(&events), true)))
                }
            }));
            let bundle = ProjectMemoryBundle::new(options);
            let staged = bundle.stage().map_err(RuntimeError::from)?;
            assert!(staged.management.is_some());
            resources.add(bundle);
            Ok(())
        }
    });
    let registry = ProjectRuntimeRegistry::new(root.join("fallback"), factory);
    let runtime = registry.resolve(Some(&root)).unwrap();
    assert_eq!(events.snapshot(), ["factory"]);
    registry.invalidate(None);
    runtime.wait_disposed().await.unwrap();
    assert_eq!(events.snapshot(), ["factory", "dispose"]);
    registry.dispose().await.unwrap();
}

#[tokio::test]
async fn missing_factory_matches_native_selection() {
    let missing = ProjectMemoryBundle::new(ProjectMemoryBundleOptions::new(PROJECT));
    let resources = missing.stage().unwrap();
    assert!(resources.memory.is_none());
    missing.dispose().await.unwrap();

    let mut enabled = ProjectMemoryBundleOptions::new(PROJECT);
    enabled.config = Some(MemoryConfig {
        enabled: true,
        provider: "edgeclaw".into(),
    });
    let bundle = ProjectMemoryBundle::new(enabled);
    assert_eq!(
        bundle.stage().unwrap_err(),
        MemoryBundleError::NativeProviderNotPorted
    );
    bundle.dispose().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_dispose_waiters_observe_one_completion() {
    let disposer_calls = Arc::new(AtomicUsize::new(0));
    let events = Arc::new(Events::default());
    let mut options = ProjectMemoryBundleOptions::new(PROJECT);
    options.provider_factory = Some(Arc::new({
        let disposer_calls = Arc::clone(&disposer_calls);
        let events = Arc::clone(&events);
        move |_: ProjectMemoryBundleOptions| {
            let disposer_calls = Arc::clone(&disposer_calls);
            let events = Arc::clone(&events);
            Ok(Some(ProjectMemoryProvider {
                memory: Arc::new(FakeResolver),
                maintenance: None,
                management: None,
                dispose: Some(Arc::new(move || {
                    let disposer_calls = Arc::clone(&disposer_calls);
                    let events = Arc::clone(&events);
                    Box::pin(async move {
                        disposer_calls.fetch_add(1, Ordering::SeqCst);
                        events.push("dispose");
                        tokio::task::yield_now().await;
                        Ok(())
                    })
                })),
            }))
        }
    }));
    let bundle = Arc::new(ProjectMemoryBundle::new(options));
    bundle.stage().unwrap();

    let mut waiters = Vec::new();
    for _ in 0..32 {
        let bundle = Arc::clone(&bundle);
        waiters.push(tokio::spawn(async move { bundle.dispose().await }));
    }
    let completion = async {
        for waiter in waiters {
            waiter.await.unwrap().unwrap();
        }
        // A caller that starts after completion also observes the same result.
        bundle.dispose().await.unwrap();
    };
    tokio::time::timeout(std::time::Duration::from_secs(10), completion)
        .await
        .expect("every dispose caller must complete");
    assert_eq!(
        disposer_calls.load(Ordering::SeqCst),
        1,
        "the disposer must run exactly once"
    );
    assert_eq!(events.snapshot(), ["dispose"]);
}
