//! Replay of the real-TS memory fixture: ProjectMemoryBundle stage/dispose and
//! the createLocalGateway memoryList/memoryWipe projection.
#[path = "common/baseline.rs"]
mod baseline;

use pilotdeck_gateway::runtime::{
    context::memory::{
        MemoryCaptureFuture, MemoryCaptureTurnInput, MemoryFuture as RetrieveFuture,
        MemoryRetrieveInput, MemoryRetrieveResult,
    },
    memory::{
        MemoryBundleError, MemoryCaptureStrategy, MemoryConfig, MemoryDisposeFn, MemoryFuture,
        MemoryListInput, MemoryMaintenanceService, MemoryManagementService, MemoryModelConfig,
        MemoryModelProvider, MemoryProjection, MemoryProjectionError, MemoryProviderError,
        MemoryResolver, MemoryScheduleConfig, MemoryWipeInput, NativeMemoryProviderOptions,
        NativeMemoryService, NativeProjectMemoryProvider, ProjectMemoryBundle,
        ProjectMemoryBundleOptions, ProjectMemoryProvider, ProjectMemoryRegistry,
        ProjectMemoryResources,
    },
    project::{
        ProjectRuntimeRegistry, ResourceBundle, RuntimeError, RuntimeResult, TelemetryClient,
    },
};
use pilotdeck_protocol::{JsObject, JsString, JsValue, parse_json, stringify_json};
use serde_json::{Value, json};
use std::{
    future::Future,
    path::{Path, PathBuf},
    pin::Pin,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, UNIX_EPOCH},
};

const ORACLE: &str = include_str!("fixtures/memory/oracle.json");
const PROJECT: &str = "/pdrs-fixture/project";

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
// The bundle replay only carries the handle; retrieval is the context
// package's contract, so this stub is never invoked.
impl MemoryResolver for FakeResolver {
    fn capture_turn(&self, _input: MemoryCaptureTurnInput) -> MemoryCaptureFuture {
        Box::pin(async { Ok(()) })
    }
    fn retrieve(&self, _input: MemoryRetrieveInput) -> RetrieveFuture {
        Box::pin(async { Ok(MemoryRetrieveResult::default()) })
    }
}
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
    fn clear_session(&self, _: &JsString) -> Result<(), MemoryProviderError> {
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
    fn clear_session(&self, _: &JsString) -> Result<(), MemoryProviderError> {
        Ok(())
    }
    fn close(&self) -> Result<(), MemoryProviderError> {
        self.events.push("close");
        Ok(())
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

/// Project an error the way a JS consumer sees it: take the real
/// `name`/`code`/`message` (and `requires_host`) from the retained JS shape
/// instead of hardcoding `name:"Error"`. Omitting `requires_host` when false
/// keeps the committed cases' oracle bytes stable.
fn error_shape_json(error: &MemoryProjectionError) -> Value {
    let mut shape = serde_json::Map::new();
    shape.insert("name".into(), Value::String(error.name().to_owned()));
    shape.insert(
        "code".into(),
        error
            .code()
            .map_or(Value::Null, |code| Value::String(code.to_owned())),
    );
    shape.insert(
        "message".into(),
        Value::String(
            error
                .message()
                .to_utf8()
                .expect("fixture error messages are valid UTF-8"),
        ),
    );
    if error.requires_host() {
        shape.insert("requires_host".into(), Value::Bool(true));
    }
    Value::Object(shape)
}

fn bundle_error_json(error: &MemoryBundleError) -> Value {
    error_shape_json(&MemoryProjectionError::from(error.clone()))
}

fn runtime_error_json(error: &RuntimeError) -> Value {
    error_shape_json(&MemoryProjectionError::from(error.clone()))
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

fn json_text(value: &JsString) -> String {
    value.to_utf8().expect("fixture strings are valid UTF-8")
}

/// Emit an integral `f64` as a JSON integer so the value equals the fixture's
/// `321`/`7` (serde_json's `Number(321.0)` is not `Number(321)`).
fn json_number(value: Option<f64>) -> Value {
    let Some(value) = value else {
        return Value::Null;
    };
    if value.fract() == 0.0 && value >= i64::MIN as f64 && value <= i64::MAX as f64 {
        Value::Number((value as i64).into())
    } else {
        serde_json::Number::from_f64(value).map_or(Value::Null, Value::Number)
    }
}

fn config_json(config: &MemoryConfig) -> Value {
    json!({
        "enabled": config.enabled,
        "provider": json_text(&config.provider),
        "rootDir": json_text(&config.root_dir),
        "captureStrategy": config.capture_strategy.as_str(),
        "includeAssistant": config.include_assistant,
        "maxMessageChars": json_number(config.max_message_chars),
        "retrievalTimeoutMs": json_number(config.retrieval_timeout_ms),
        "model": config.model.as_ref().map(json_text),
        "apiType": config.api_type.as_ref().map(json_text),
        "schedule": config.schedule.as_ref().map(|schedule| json!({
            "reasoningMode": schedule.reasoning_mode.as_ref().map(json_text),
            "autoIndexIntervalMinutes": json_number(schedule.auto_index_interval_minutes),
            "autoDreamIntervalMinutes": json_number(schedule.auto_dream_interval_minutes),
        })),
        "heartbeatBatchSize": json_number(config.heartbeat_batch_size),
    })
}

fn model_config_json(config: &MemoryModelConfig) -> Value {
    Value::Object(
        config
            .providers
            .iter()
            .map(|(id, provider)| {
                (
                    id.clone(),
                    json!({
                        "protocol": provider.protocol.as_ref().map(json_text),
                        "url": provider.url.as_ref().map(json_text),
                        "apiKey": provider.api_key.as_ref().map(json_text),
                    }),
                )
            })
            .collect(),
    )
}

/// Full recorder projection (`captureOptions` in generate.mts).
fn options_json(options: &ProjectMemoryBundleOptions) -> Value {
    json!({
        "projectRoot": options.project_root.to_string_lossy(),
        "agentModel": options.agent_model,
        "hasProviderFactory": options.provider_factory.is_some(),
        "hasCreateProvider": options.create_provider.is_some(),
        "config": options.config.as_ref().map(config_json),
        "modelConfig": options.model_config.as_ref().map(model_config_json),
        "hasNow": options.now.is_some(),
        "hasTelemetry": options.telemetry.is_some(),
    })
}

/// The pinned two-field recorder used by the ten committed cases.
fn committed_options_json(options: &ProjectMemoryBundleOptions) -> Value {
    json!({
        "projectRoot": options.project_root.to_string_lossy(),
        "agentModel": options.agent_model,
        "hasProviderFactory": options.provider_factory.is_some(),
        "hasCreateProvider": options.create_provider.is_some(),
        "config": options.config.as_ref().map(|config| json!({
            "enabled": config.enabled,
            "provider": json_text(&config.provider),
        })),
    })
}

fn native_options_json(input: &NativeMemoryProviderOptions) -> Value {
    json!({
        "projectRoot": input.project_root.to_string_lossy(),
        "agentModel": input.agent_model,
        "hasProviderFactory": false,
        "hasCreateProvider": false,
        "config": input.config.as_ref().map(config_json),
        "modelConfig": input.model_config.as_ref().map(model_config_json),
        "hasNow": input.now.is_some(),
        "hasTelemetry": input.telemetry.is_some(),
    })
}

fn committed_native_options_json(input: &NativeMemoryProviderOptions) -> Value {
    json!({
        "projectRoot": input.project_root.to_string_lossy(),
        "agentModel": input.agent_model,
        "hasProviderFactory": false,
        "hasCreateProvider": false,
        "config": input.config.as_ref().map(|config| json!({
            "enabled": config.enabled,
            "provider": json_text(&config.provider),
        })),
    })
}

fn memory_config_from_fixture(config: &Value) -> MemoryConfig {
    MemoryConfig {
        enabled: config["enabled"].as_bool().expect("config.enabled"),
        provider: JsString::from(config["provider"].as_str().expect("config.provider")),
        root_dir: config
            .get("rootDir")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .into(),
        capture_strategy: match config.get("captureStrategy").and_then(Value::as_str) {
            Some("full_session") => MemoryCaptureStrategy::FullSession,
            _ => MemoryCaptureStrategy::LastTurn,
        },
        include_assistant: config
            .get("includeAssistant")
            .and_then(Value::as_bool)
            .unwrap_or(true),
        max_message_chars: config.get("maxMessageChars").and_then(Value::as_f64),
        retrieval_timeout_ms: config.get("retrievalTimeoutMs").and_then(Value::as_f64),
        model: config
            .get("model")
            .and_then(Value::as_str)
            .map(JsString::from),
        api_type: config
            .get("apiType")
            .and_then(Value::as_str)
            .map(JsString::from),
        schedule: config
            .get("schedule")
            .and_then(Value::as_object)
            .map(|schedule| MemoryScheduleConfig {
                reasoning_mode: schedule
                    .get("reasoningMode")
                    .and_then(Value::as_str)
                    .map(JsString::from),
                auto_index_interval_minutes: schedule
                    .get("autoIndexIntervalMinutes")
                    .and_then(Value::as_f64),
                auto_dream_interval_minutes: schedule
                    .get("autoDreamIntervalMinutes")
                    .and_then(Value::as_f64),
            }),
        heartbeat_batch_size: config.get("heartbeatBatchSize").and_then(Value::as_f64),
    }
}

fn model_config_from_fixture(model_config: &Value) -> MemoryModelConfig {
    MemoryModelConfig {
        providers: model_config
            .as_object()
            .expect("modelConfig providers")
            .iter()
            .map(|(id, provider)| {
                (
                    id.clone(),
                    MemoryModelProvider {
                        protocol: provider
                            .get("protocol")
                            .and_then(Value::as_str)
                            .map(JsString::from),
                        url: provider
                            .get("url")
                            .and_then(Value::as_str)
                            .map(JsString::from),
                        api_key: provider
                            .get("apiKey")
                            .and_then(Value::as_str)
                            .map(JsString::from),
                    },
                )
            })
            .collect(),
    }
}

fn options_from_fixture(case: &Value) -> ProjectMemoryBundleOptions {
    let options = &case["options"];
    let project_root = options["projectRoot"].as_str().unwrap();
    let mut bundle = ProjectMemoryBundleOptions::new(project_root);
    if let Some(config) = options.get("config").filter(|value| !value.is_null()) {
        bundle.config = Some(memory_config_from_fixture(config));
    }
    if let Some(model_config) = options.get("modelConfig").filter(|value| !value.is_null()) {
        bundle.model_config = Some(model_config_from_fixture(model_config));
    }
    bundle.agent_model = options["agentModel"].as_str().map(str::to_owned);
    if options["hasNow"].as_bool() == Some(true) {
        bundle.now = Some(Arc::new(|| UNIX_EPOCH + Duration::from_millis(1234)));
    }
    if options["hasTelemetry"].as_bool() == Some(true) {
        let telemetry: TelemetryClient = Arc::new(());
        bundle.telemetry = Some(telemetry);
    }
    bundle
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
    // Cases appended by fix-r1 record the full option surface; the ten
    // committed cases keep their pinned two-field recorder.
    let full = case.get("recorder").and_then(Value::as_str) == Some("full");
    match case["factory"].as_str().unwrap() {
        "success" => {
            options.provider_factory = Some(Arc::new(move |input: ProjectMemoryBundleOptions| {
                factory_calls.fetch_add(1, Ordering::SeqCst);
                *captured_factory.lock().unwrap() = Some(if full {
                    options_json(&input)
                } else {
                    committed_options_json(&input)
                });
                events.push("factory");
                Ok(Some(provider_with_events(Arc::clone(&events), true)))
            }));
        }
        "throws" => {
            let factory_events = Arc::clone(&events);
            options.provider_factory = Some(Arc::new(move |_: ProjectMemoryBundleOptions| {
                factory_calls.fetch_add(1, Ordering::SeqCst);
                factory_events.push("factory");
                Err(MemoryBundleError::Factory(MemoryProviderError::new(
                    "factory boom",
                )))
            }));
        }
        "throws-structured" => {
            let factory_events = Arc::clone(&events);
            options.provider_factory = Some(Arc::new(move |_: ProjectMemoryBundleOptions| {
                factory_calls.fetch_add(1, Ordering::SeqCst);
                factory_events.push("factory");
                Err(MemoryBundleError::Factory(MemoryProviderError {
                    name: "DialogGatewayError".into(),
                    code: Some("FACTORY_FAILED".into()),
                    message: JsString::from("structured factory boom"),
                    requires_host: true,
                }))
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
                *captured_native.lock().unwrap() = Some(if full {
                    native_options_json(input)
                } else {
                    committed_native_options_json(input)
                });
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
                *captured_factory.lock().unwrap() = Some(if full {
                    options_json(&input)
                } else {
                    committed_options_json(&input)
                });
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
    let oracle = baseline::oracle(ORACLE);
    assert_eq!(
        oracle
            .get("sourceSha256")
            .and_then(JsValue::as_object)
            .unwrap()
            .len(),
        728,
        "#303 exact import closure source count"
    );
    assert_eq!(
        oracle
            .get("projectionCases")
            .and_then(as_array)
            .unwrap()
            .len(),
        10
    );
    // Bundle expectations contain no lone surrogates, so they can be replayed
    // through serde_json like before; the projection cases below stay in
    // JsValue because their project keys carry raw UTF-16 units.
    let bundle = stringify_json(oracle.get("bundleCases").unwrap())
        .unwrap()
        .unwrap();
    let bundle: Value = serde_json::from_str(&bundle).unwrap();
    let cases = bundle.as_array().unwrap();
    assert_eq!(cases.len(), 13);
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
    assert_eq!(replayed, 13);
}

fn optional_js_string(input: &JsValue, key: &str) -> Option<JsString> {
    input.get(key).and_then(JsValue::as_string).cloned()
}
fn as_array(value: &JsValue) -> Option<&Vec<JsValue>> {
    match value {
        JsValue::Array(items) => Some(items),
        _ => None,
    }
}
fn text(value: &JsValue) -> Option<String> {
    value.as_string().and_then(|value| value.to_utf8().ok())
}
fn call_object(op: &str) -> JsObject {
    let mut object = JsObject::new();
    object.insert("op".into(), JsValue::from(op));
    object
}

struct ProjectionRegistry {
    calls: Arc<Mutex<Vec<JsValue>>>,
    management: Option<Arc<dyn MemoryManagementService>>,
    resolve_key_fail: Option<String>,
    resolve_fail: Option<String>,
    /// `knownProjects` from the fixture: mimic the real registry's unit-exact
    /// match and `Unknown projectKey` rejection. `None` keeps the older mock's
    /// `resolved:<key>` echo.
    known_projects: Option<Vec<JsString>>,
}
impl ProjectMemoryRegistry for ProjectionRegistry {
    fn resolve_project_key(&self, project_key: &JsString) -> Result<JsString, MemoryProviderError> {
        let mut call = call_object("resolveProjectKey");
        call.insert("key".into(), JsValue::String(project_key.clone()));
        self.calls.lock().unwrap().push(JsValue::Object(call));
        if let Some(message) = &self.resolve_key_fail {
            return Err(MemoryProviderError::new(message.clone()));
        }
        match &self.known_projects {
            Some(known) => match known
                .iter()
                .find(|known| known.units() == project_key.units())
            {
                Some(known) => Ok(known.clone()),
                None => Err(MemoryProviderError::unknown_project_key(
                    project_key.units(),
                )),
            },
            None => {
                let mut units = "resolved:".encode_utf16().collect::<Vec<_>>();
                units.extend_from_slice(project_key.units());
                Ok(JsString::from_utf16(units))
            }
        }
    }
    fn memory_management(
        &self,
        project_key: &JsString,
    ) -> Result<Option<Arc<dyn MemoryManagementService>>, MemoryProviderError> {
        let mut call = call_object("resolve");
        call.insert("key".into(), JsValue::String(project_key.clone()));
        self.calls.lock().unwrap().push(JsValue::Object(call));
        if let Some(message) = &self.resolve_fail {
            return Err(MemoryProviderError::new(message.clone()));
        }
        Ok(self.management.clone())
    }
}

struct ProjectionManagement {
    calls: Arc<Mutex<Vec<JsValue>>>,
    items: Vec<JsValue>,
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
        let mut call = call_object("list");
        call.insert("limit".into(), JsValue::Number(limit as f64));
        self.calls.lock().unwrap().push(JsValue::Object(call));
        self.maybe_fail("list")?;
        Ok(self.items.clone())
    }
    fn clear(&self, scope: &str) -> Result<(), MemoryProviderError> {
        let mut call = call_object("clear");
        call.insert("scope".into(), JsValue::from(scope));
        self.calls.lock().unwrap().push(JsValue::Object(call));
        self.maybe_fail("clear")
    }
    fn clear_session(&self, session_key: &JsString) -> Result<(), MemoryProviderError> {
        let mut call = call_object("clearSession");
        call.insert("sessionKey".into(), JsValue::String(session_key.clone()));
        self.calls.lock().unwrap().push(JsValue::Object(call));
        self.maybe_fail("clearSession")
    }
}

#[test]
fn real_ts_memory_projection_oracle() {
    let oracle = baseline::oracle(ORACLE);
    let cases = oracle.get("projectionCases").and_then(as_array).unwrap();
    assert_eq!(cases.len(), 10);
    let mut replayed = 0;
    for case in cases {
        let name = text(case.get("name").unwrap()).unwrap();
        let calls = Arc::new(Mutex::new(Vec::new()));
        let fail = case.get("fail").map(|fail| {
            (
                text(fail.get("op").expect("fail op")).expect("fail op text"),
                text(fail.get("message").expect("fail message")).expect("fail message text"),
            )
        });
        let known_projects = case.get("knownProjects").map(|projects| {
            as_array(projects)
                .expect("knownProjects array")
                .iter()
                .map(|project| project.as_string().expect("known project").clone())
                .collect::<Vec<_>>()
        });
        let management: Option<Arc<dyn MemoryManagementService>> =
            if text(case.get("management").unwrap()).as_deref() == Some("available") {
                Some(Arc::new(ProjectionManagement {
                    calls: Arc::clone(&calls),
                    items: case.get("items").and_then(as_array).expect("items").clone(),
                    fail,
                }))
            } else {
                None
            };
        let registry = Arc::new(ProjectionRegistry {
            calls: Arc::clone(&calls),
            management,
            resolve_key_fail: case.get("resolveProjectKeyFails").and_then(text),
            resolve_fail: case.get("registryResolveFails").and_then(text),
            known_projects,
        });
        let projection = MemoryProjection::new(registry, "/pdrs-fixture/fallback");
        for step in case.get("steps").and_then(as_array).unwrap() {
            calls.lock().unwrap().clear();
            let input = step.get("input").expect("step input");
            let actual: Result<JsValue, JsValue> = match text(step.get("op").unwrap()).as_deref() {
                Some("list") => projection
                    .list(&MemoryListInput {
                        project_key: optional_js_string(input, "projectKey"),
                        session_key: optional_js_string(input, "sessionKey"),
                    })
                    .map(|result| result.to_js())
                    .map_err(|error| error.to_js()),
                Some("wipe") => projection
                    .wipe(&MemoryWipeInput {
                        project_key: optional_js_string(input, "projectKey"),
                        session_key: optional_js_string(input, "sessionKey"),
                        scope: optional_js_string(input, "scope"),
                    })
                    .map(|result| result.to_js())
                    .map_err(|error| error.to_js()),
                other => panic!("unknown projection step {other:?}"),
            };
            if let Some(result) = step.get("resultJson") {
                let expected = parse_json(
                    result
                        .as_string()
                        .expect("resultJson")
                        .to_utf8()
                        .unwrap()
                        .as_str(),
                )
                .unwrap();
                let actual =
                    actual.unwrap_or_else(|error| panic!("{name}: unexpected error {error:?}"));
                assert_eq!(actual, expected, "{name}: result");
            } else {
                let expected = step.get("error").expect("step error").clone();
                let actual = match actual {
                    Ok(value) => panic!("{name}: unexpected success {value:?}"),
                    Err(error) => error,
                };
                assert_eq!(actual, expected, "{name}: error");
            }
            assert_eq!(
                JsValue::Array(calls.lock().unwrap().clone()),
                *step.get("calls").expect("step calls"),
                "{name}: calls"
            );
        }
        replayed += 1;
    }
    assert_eq!(replayed, 10);
}

struct PanicRegistry;
impl ProjectMemoryRegistry for PanicRegistry {
    fn resolve_project_key(&self, _: &JsString) -> Result<JsString, MemoryProviderError> {
        panic!("a non-UTF-8 fallback must not reach the registry")
    }
    fn memory_management(
        &self,
        _: &JsString,
    ) -> Result<Option<Arc<dyn MemoryManagementService>>, MemoryProviderError> {
        unreachable!("resolve_project_key runs first")
    }
}

#[test]
fn non_utf8_fallback_is_a_host_boundary() {
    use std::os::unix::ffi::OsStringExt;
    let fallback = PathBuf::from(std::ffi::OsString::from_vec(vec![b'/', 0xFF]));
    assert!(fallback.to_str().is_none());
    let projection = MemoryProjection::new(Arc::new(PanicRegistry), fallback);
    let error = projection.list(&MemoryListInput::default()).unwrap_err();
    assert_eq!(error.name(), "Error");
    assert_eq!(error.code(), Some("JS_HOST_REQUIRED"));
    assert!(error.requires_host());
    assert!(error.message().units().is_empty());
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
            options.config = Some(MemoryConfig::new(true, "edgeclaw"));
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

    let scratch = Scratch::new();
    let mut enabled = ProjectMemoryBundleOptions::new(scratch.0.join("project"));
    let mut config = MemoryConfig::new(true, "edgeclaw");
    config.root_dir = scratch.0.join("store").to_string_lossy().as_ref().into();
    enabled.config = Some(config);
    let bundle = ProjectMemoryBundle::new(enabled);
    let resources = bundle.stage().expect("#778 native default construction");
    assert!(resources.memory.is_some(), "#778 native default resolver");
    assert!(
        resources.maintenance.is_some(),
        "#778 native default maintenance"
    );
    assert!(
        resources.management.is_some(),
        "#778 native default management"
    );
    bundle.dispose().await.expect("#778 native default close");
    bundle
        .dispose()
        .await
        .expect("#778 native default close once");
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

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn dispose_during_factory_schedules_the_published_disposer() {
    let entered = Arc::new(tokio::sync::Notify::new());
    let entered_signal = Arc::clone(&entered);
    let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
    // `Receiver` is `Send` but not `Sync`; the factory must be `Fn + Sync`, so
    // the receiver is captured behind a mutex (used exactly once).
    let release_rx = Mutex::new(release_rx);
    let disposer_calls = Arc::new(AtomicUsize::new(0));
    let calls = Arc::clone(&disposer_calls);
    let mut options = ProjectMemoryBundleOptions::new("/pdrs-fixture/stage-race");
    options.provider_factory = Some(Arc::new(move |_| {
        entered_signal.notify_one();
        release_rx
            .lock()
            .unwrap()
            .recv_timeout(std::time::Duration::from_secs(10))
            .expect("test must release the factory");
        // Clone before building the inner `Fn` disposer: the factory closure
        // itself may not move its captures out.
        let calls = Arc::clone(&calls);
        Ok(Some(ProjectMemoryProvider {
            memory: Arc::new(FakeResolver),
            maintenance: None,
            management: None,
            dispose: Some(Arc::new(move || {
                let calls = Arc::clone(&calls);
                Box::pin(async move {
                    calls.fetch_add(1, Ordering::SeqCst);
                    Ok(())
                })
            })),
        }))
    }));
    let bundle = Arc::new(ProjectMemoryBundle::new(options));
    let staged = {
        let bundle = Arc::clone(&bundle);
        tokio::task::spawn_blocking(move || bundle.stage())
    };
    entered.notified().await;
    let dispose = bundle.dispose();
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    assert_eq!(
        disposer_calls.load(Ordering::SeqCst),
        0,
        "dispose must wait for the in-flight stage"
    );
    release_tx.send(()).unwrap();
    let resources = staged.await.unwrap().unwrap();
    assert_eq!(
        resources.js_keys(),
        ["memory", "memoryManagement", "memoryService"]
    );
    tokio::time::timeout(std::time::Duration::from_secs(10), dispose)
        .await
        .expect("the published disposer must run")
        .unwrap();
    assert_eq!(disposer_calls.load(Ordering::SeqCst), 1);
    bundle.dispose().await.unwrap();
    assert_eq!(
        disposer_calls.load(Ordering::SeqCst),
        1,
        "the disposer must run exactly once"
    );
}

/// A Rust panic is not a TS throw, but it must not wedge a concurrent
/// `dispose()`: the publication guard has to be alive while the factory runs.
#[test]
fn panicking_factory_still_releases_a_waiting_dispose() {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let entered = Arc::new(tokio::sync::Notify::new());
        let entered_signal = Arc::clone(&entered);
        let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
        let release_rx = Mutex::new(release_rx);
        let mut options = ProjectMemoryBundleOptions::new("/pdrs-fixture/stage-panic");
        options.provider_factory = Some(Arc::new(move |_| {
            entered_signal.notify_one();
            release_rx
                .lock()
                .unwrap()
                .recv_timeout(std::time::Duration::from_secs(10))
                .expect("test must release the factory");
            panic!("factory boom");
        }));
        let bundle = Arc::new(ProjectMemoryBundle::new(options));
        let staged = {
            let bundle = Arc::clone(&bundle);
            tokio::task::spawn_blocking(move || {
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| bundle.stage()))
            })
        };
        entered.notified().await;
        let dispose = bundle.dispose();
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        release_tx.send(()).unwrap();
        let staged = staged.await.unwrap();
        assert!(staged.is_err(), "the factory panic must propagate");
        tokio::time::timeout(std::time::Duration::from_secs(10), dispose)
            .await
            .expect("dispose must not wait forever for a panicked stage")
            .unwrap();
    });
}

/// Production `MemoryProjection` over a real `ProjectRuntimeRegistry`, with
/// only the project-key resolver stubbed. A factory-stage rejection must reach
/// the final consumer with the original JS shape.
struct StagedRuntimeRegistry {
    registry: Arc<ProjectRuntimeRegistry>,
}
impl ProjectMemoryRegistry for StagedRuntimeRegistry {
    fn resolve_project_key(&self, key: &JsString) -> Result<JsString, MemoryProviderError> {
        Ok(key.clone())
    }
    fn memory_management(
        &self,
        _project_key: &JsString,
    ) -> Result<Option<Arc<dyn MemoryManagementService>>, MemoryProviderError> {
        self.registry
            .resolve(Some(Path::new(PROJECT)))
            .map_err(MemoryProviderError::from)?;
        Ok(None)
    }
}

#[tokio::test]
async fn runtime_stage_error_reaches_memory_projection() {
    let message = JsString::from_utf16(
        "factory boom "
            .encode_utf16()
            .chain([0xD800])
            .collect::<Vec<_>>(),
    );
    let thrown = message.clone();
    let factory = Arc::new(move |root: &Path, _: &mut ResourceBundle| {
        let thrown = thrown.clone();
        let mut options = ProjectMemoryBundleOptions::new(root);
        options.provider_factory = Some(Arc::new(move |_| {
            Err(MemoryBundleError::Factory(MemoryProviderError {
                name: "DialogGatewayError".into(),
                code: Some("FACTORY_FAILED".into()),
                message: thrown.clone(),
                requires_host: true,
            }))
        }));
        ProjectMemoryBundle::new(options)
            .stage()
            .map_err(RuntimeError::from)?;
        Ok(())
    });
    let registry = Arc::new(ProjectRuntimeRegistry::new(PROJECT, factory));
    let projection = MemoryProjection::new(
        Arc::new(StagedRuntimeRegistry {
            registry: Arc::clone(&registry),
        }),
        PROJECT,
    );

    let error = projection
        .list(&MemoryListInput {
            project_key: Some(JsString::from(PROJECT)),
            session_key: None,
        })
        .expect_err("the factory rejection must stop memory_list");
    assert_eq!(error.name(), "DialogGatewayError");
    assert_eq!(error.code(), Some("FACTORY_FAILED"));
    assert_eq!(error.message().units(), message.units());
    assert!(!error.message().units().contains(&0xFFFD));
    assert!(error.requires_host());
    registry.dispose().await.unwrap();
}

#[test]
fn factory_error_projects_name_code_and_raw_utf16_units() {
    let message = JsString::from_utf16(
        "factory boom "
            .encode_utf16()
            .chain([0xD800])
            .collect::<Vec<_>>(),
    );
    let mut options = ProjectMemoryBundleOptions::new("/pdrs-fixture/factory-error");
    let thrown = message.clone();
    options.provider_factory = Some(Arc::new(move |_| {
        Err(MemoryBundleError::Factory(MemoryProviderError {
            name: "DialogGatewayError".into(),
            code: Some("FACTORY_FAILED".into()),
            message: thrown.clone(),
            requires_host: false,
        }))
    }));
    let bundle = ProjectMemoryBundle::new(options);
    let error = bundle.stage().unwrap_err();
    assert_eq!(
        error
            .factory_error()
            .expect("factory error retains its JS shape")
            .message
            .units(),
        message.units()
    );
    let projected = MemoryProjectionError::from(error);
    assert_eq!(projected.name(), "DialogGatewayError");
    assert_eq!(projected.code(), Some("FACTORY_FAILED"));
    assert_eq!(projected.message().units(), message.units());
    assert!(!projected.message().units().contains(&0xFFFD));
    assert!(!projected.requires_host());
}

/// #318 item 3: `sessionKey` is a JS-owned wire string. A lone surrogate must
/// filter and reach `clearSession` as the same UTF-16 units, never aliasing the
/// literal U+FFFD string.
#[test]
fn session_key_identity_is_utf16_exact() {
    let lone = JsString::from_utf16(
        "a".encode_utf16()
            .chain([0xD800])
            .chain("b".encode_utf16())
            .collect::<Vec<_>>(),
    );
    let items = vec![
        parse_json(r#"{"id":"surrogate","sourceSessionKey":"a\ud800b"}"#).unwrap(),
        parse_json(r#"{"id":"replacement","sourceSessionKey":"a\ufffdb"}"#).unwrap(),
    ];
    let calls = Arc::new(Mutex::new(Vec::new()));
    let management = Arc::new(ProjectionManagement {
        calls: Arc::clone(&calls),
        items,
        fail: None,
    });
    let registry = Arc::new(ProjectionRegistry {
        calls: Arc::clone(&calls),
        management: Some(management),
        resolve_key_fail: None,
        resolve_fail: None,
        known_projects: None,
    });
    let projection = MemoryProjection::new(registry, "/pdrs-fixture/fallback");

    let listed = projection
        .list(&MemoryListInput {
            project_key: Some(JsString::from("/pdrs-fixture/project")),
            session_key: Some(lone.clone()),
        })
        .expect("list with a lone surrogate");
    assert_eq!(listed.items.len(), 1, "must not alias the U+FFFD item");
    assert_eq!(
        listed.items[0]
            .get("id")
            .and_then(JsValue::as_string)
            .and_then(|id| id.to_utf8().ok())
            .as_deref(),
        Some("surrogate")
    );

    projection
        .wipe(&MemoryWipeInput {
            project_key: Some(JsString::from("/pdrs-fixture/project")),
            session_key: Some(lone.clone()),
            scope: Some(JsString::from("session")),
        })
        .expect("a lone surrogate is not JS whitespace");
    let calls = calls.lock().unwrap();
    let clear_session = calls
        .iter()
        .rev()
        .find(|call| {
            call.get("op")
                .and_then(JsValue::as_string)
                .is_some_and(|op| op.equals_str("clearSession"))
        })
        .expect("clearSession was called");
    assert_eq!(
        clear_session
            .get("sessionKey")
            .and_then(JsValue::as_string)
            .expect("sessionKey recorded")
            .units(),
        lone.units()
    );
}

#[test]
fn memory_dispose_completion_survives_stopped_executor() {
    use std::{
        future::Future,
        task::{Context, Poll, Waker},
    };
    let executor = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let events = Arc::new(Events::default());
    let mut options = ProjectMemoryBundleOptions::new(PROJECT);
    options.provider_factory = Some(Arc::new({
        let events = Arc::clone(&events);
        move |_| Ok(Some(provider_with_events(Arc::clone(&events), false)))
    }));
    let bundle = Arc::new(ProjectMemoryBundle::new(options));
    bundle.stage().unwrap();
    let mut pending = {
        let _entered = executor.enter();
        Box::pin(bundle.dispose())
    };
    {
        let _entered = executor.enter();
        assert!(matches!(
            pending
                .as_mut()
                .poll(&mut Context::from_waker(Waker::noop())),
            Poll::Pending
        ));
    }
    drop(executor); // the disposer task is queued but never polled
    let observer = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    observer.block_on(async {
        let error = tokio::time::timeout(Duration::from_secs(1), pending)
            .await
            .expect("memory dispose completion must survive stopped executor")
            .unwrap_err();
        assert_eq!(
            error.message,
            "Memory provider dispose task was dropped or panicked before completion."
        );
        let repeated = bundle.dispose().await.unwrap_err();
        assert_eq!(repeated.message, error.message);
    });
    assert_eq!(events.snapshot(), Vec::<String>::new());
}
