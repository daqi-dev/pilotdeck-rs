//! Replays the real TypeScript `GatewayRuntimeRefreshBundle` call sequences
//! (`tests/fixtures/runtime-refresh/generate.mts`). Every collaborator is a
//! recording fake mirroring the generator's; `real-store-*` scenarios drive
//! the bundle from the product `ConfigStore` over the same YAML files. Each
//! step's result object (with the calls it caused, in order) is rebuilt with
//! the TS key order and compared byte for byte after JSON serialization.
#[path = "common/baseline.rs"]
mod baseline;
#[path = "common/scratch.rs"]
mod scratch;

use pilotdeck_gateway::{
    config::{
        Env, Error as ConfigError,
        changes::ChangeClass,
        store::{ConfigStore, StoreOptions},
    },
    runtime::{
        project::{RuntimeError, RuntimeResult},
        refresh::{
            BoxFuture, ConfigChangeNotice, ConfigListener, ConfigReloadResult, ExtensionsReload,
            MemoryDiagnostics, MemoryMaintenance, NotificationServer, RefreshBundle,
            RefreshConfigStore, RefreshLog, RefreshOptions, RefreshRegistry, RefreshRouter,
            ReloadOutcome, SdkConfigChange, TurnCompleted, Unsubscribe, WarnDetail,
        },
    },
};
use pilotdeck_protocol::{JsObject, JsString, JsValue, stringify_json};
use scratch::Scratch;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};
use tokio::sync::oneshot;

const ORACLE: &str = include_str!("fixtures/runtime-refresh/oracle.json");
const SCENARIOS: usize = 23;
const STEPS: usize = 157;

type Calls = Arc<Mutex<Vec<JsValue>>>;

fn object(entries: Vec<(&str, JsValue)>) -> JsValue {
    let mut out = JsObject::new();
    for (key, value) in entries {
        out.insert(key.into(), value);
    }
    JsValue::Object(out)
}
fn string(value: &JsString) -> JsValue {
    JsValue::String(value.clone())
}
fn strings(values: &[JsString]) -> JsValue {
    JsValue::Array(values.iter().map(string).collect())
}
fn record(calls: &Calls, name: &str, mut args: Vec<(&str, JsValue)>) {
    args.insert(0, ("call", name.into()));
    calls.lock().unwrap().push(object(args));
}
/// The generator's `normalize`: undefined becomes an explicit tag.
fn normalize(value: &JsValue) -> JsValue {
    match value {
        JsValue::Undefined => object(vec![("$", "undefined".into())]),
        JsValue::Array(items) => JsValue::Array(items.iter().map(normalize).collect()),
        JsValue::Object(entries) => {
            let mut out = JsObject::new();
            for (key, value) in entries.entries() {
                out.insert(key.clone(), normalize(value));
            }
            JsValue::Object(out)
        }
        other => other.clone(),
    }
}
fn encode(value: &JsValue) -> String {
    stringify_json(value).unwrap().unwrap()
}
fn field<'a>(value: &'a JsValue, key: &str) -> Option<&'a JsValue> {
    value.as_object().and_then(|object| object.get(key))
}
fn text(value: &JsValue) -> String {
    value.as_string().unwrap().to_utf8().unwrap()
}
fn js_strings(value: &JsValue) -> Vec<JsString> {
    match value {
        JsValue::Array(items) => items
            .iter()
            .map(|item| item.as_string().unwrap().clone())
            .collect(),
        other => panic!("expected string array, got {other:?}"),
    }
}
fn classes(value: &JsValue) -> Vec<ChangeClass> {
    js_strings(value)
        .iter()
        .map(|class| {
            [
                ChangeClass::RuntimeLive,
                ChangeClass::NextRequest,
                ChangeClass::NextRuntime,
                ChangeClass::RestartRequired,
            ]
            .into_iter()
            .find(|candidate| class.equals_str(candidate.as_str()))
            .unwrap_or_else(|| panic!("unknown change class {class:?}"))
        })
        .collect()
}
fn error_record(error: &ConfigError) -> JsValue {
    // Only PilotConfigError carries a baseline `code`.
    let mut entries = vec![
        ("name", error.name.into()),
        ("message", JsValue::String(error.message.clone())),
    ];
    if error.name == "PilotConfigError" {
        entries.push(("code", error.code.as_str().into()));
    }
    object(entries)
}

#[derive(Default)]
struct Script {
    emits: Vec<(Vec<JsString>, Vec<ChangeClass>)>,
    fail: Option<String>,
}

struct FakeStore {
    calls: Calls,
    listeners: Mutex<Vec<(usize, ConfigListener)>>,
    next: AtomicUsize,
    script: Mutex<Script>,
    real: Mutex<Option<ConfigStore>>,
}
impl FakeStore {
    fn emit(&self, paths: &[JsString], classes: &[ChangeClass]) {
        let listeners: Vec<_> = self
            .listeners
            .lock()
            .unwrap()
            .iter()
            .map(|(_, l)| l.clone())
            .collect();
        for listener in listeners {
            let _ =
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| listener(paths, classes)));
        }
    }
}
struct StoreHandle(Arc<FakeStore>);
impl RefreshConfigStore for StoreHandle {
    fn subscribe(&self, listener: ConfigListener) -> Unsubscribe {
        let id = self.0.next.fetch_add(1, Ordering::SeqCst) + 1;
        record(
            &self.0.calls,
            "store.subscribe",
            vec![("id", JsValue::Number(id as f64))],
        );
        let real = self.0.real.lock().unwrap().clone();
        let unsubscribe_real = match real {
            Some(real) => Some(RefreshConfigStore::subscribe(&real, listener)),
            None => {
                self.0.listeners.lock().unwrap().push((id, listener));
                None
            }
        };
        let store = Arc::clone(&self.0);
        Box::new(move || {
            record(
                &store.calls,
                "store.unsubscribe",
                vec![("id", JsValue::Number(id as f64))],
            );
            if let Some(unsubscribe) = unsubscribe_real {
                unsubscribe();
            }
            store
                .listeners
                .lock()
                .unwrap()
                .retain(|(entry, _)| *entry != id);
        })
    }
    fn reload(&self, reason: &'static str) -> BoxFuture<ConfigReloadResult> {
        record(
            &self.0.calls,
            "store.reload",
            vec![("reason", reason.into())],
        );
        let real = self.0.real.lock().unwrap().clone();
        if let Some(real) = real {
            return RefreshConfigStore::reload(&real, reason);
        }
        let script = std::mem::take(&mut *self.0.script.lock().unwrap());
        for (paths, classes) in &script.emits {
            self.0.emit(paths, classes);
        }
        let result = match script.fail {
            Some(message) => Err(Arc::new(ConfigError {
                code: String::new(),
                message: message.as_str().into(),
                diagnostics: vec![],
                name: "Error",
                requires_host: false,
            })),
            None => Ok(()),
        };
        Box::pin(async move { result })
    }
}

struct FakeRegistry {
    calls: Calls,
    pending: Mutex<Vec<Option<oneshot::Sender<RuntimeResult>>>>,
    throws: Mutex<Option<String>>,
}
impl RefreshRegistry for FakeRegistry {
    fn reload(&self) -> Result<BoxFuture<RuntimeResult>, RuntimeError> {
        record(&self.calls, "registry.reload", vec![]);
        if let Some(message) = self.throws.lock().unwrap().clone() {
            return Err(RuntimeError::new(message));
        }
        let (sender, receiver) = oneshot::channel();
        self.pending.lock().unwrap().push(Some(sender));
        Ok(Box::pin(async move {
            receiver
                .await
                .unwrap_or_else(|_| Err(RuntimeError::new("fake reload dropped")))
        }))
    }
    fn invalidate(&self, project_key: Option<&JsString>) {
        let args = project_key.into_iter().map(string).collect();
        record(
            &self.calls,
            "registry.invalidate",
            vec![("args", JsValue::Array(args))],
        );
    }
}

struct Fake(Calls);
impl RefreshRouter for Fake {
    fn mark_all_dirty(&self, reason: &str) -> usize {
        record(
            &self.0,
            "router.markAllDirty",
            vec![("reason", reason.into())],
        );
        2
    }
    fn mark_project_dirty(&self, project_key: &JsString, reason: &str) -> usize {
        record(
            &self.0,
            "router.markProjectDirty",
            vec![
                ("projectKey", string(project_key)),
                ("reason", reason.into()),
            ],
        );
        1
    }
    fn mark_session_dirty(&self, session_key: &JsString, reason: &str) -> bool {
        record(
            &self.0,
            "router.markSessionDirty",
            vec![
                ("sessionKey", string(session_key)),
                ("reason", reason.into()),
            ],
        );
        !session_key.equals_str("missing")
    }
    fn cached_session_count(&self) -> usize {
        record(&self.0, "router.cachedSessionCount", vec![]);
        3
    }
    fn snapshot_session(&self, session_key: &JsString) -> Option<JsValue> {
        record(
            &self.0,
            "router.snapshotSession",
            vec![("sessionKey", string(session_key))],
        );
        (!session_key.equals_str("missing")).then(|| {
            JsValue::Array(vec![object(vec![
                ("role", "user".into()),
                ("key", string(session_key)),
            ])])
        })
    }
}
impl NotificationServer for Fake {
    fn broadcast_notification(&self, name: &str, payload: &JsValue) {
        record(
            &self.0,
            "server.broadcast",
            vec![("name", name.into()), ("payload", payload.clone())],
        );
    }
}
impl SdkConfigChange for Fake {
    fn dispatch_sdk_config_change(&self, change: &ConfigChangeNotice) {
        record(
            &self.0,
            "sdk.configChange",
            vec![("payload", change.to_js())],
        );
    }
}
impl MemoryMaintenance for Fake {
    fn schedule(&self, project_key: &JsString) -> RuntimeResult {
        record(
            &self.0,
            "memory.schedule",
            vec![("args", JsValue::Array(vec![string(project_key)]))],
        );
        Ok(())
    }
}
impl MemoryDiagnostics for Fake {
    fn log_memory_diagnostic(&self, input: JsValue) {
        record(&self.0, "memory.diagnostic", vec![("input", input)]);
    }
    fn summarize_messages(&self, messages: &JsValue) -> JsObject {
        record(
            &self.0,
            "memory.summarize",
            vec![("messages", messages.clone())],
        );
        let JsValue::Array(items) = messages else {
            panic!("messages")
        };
        let mut summary = JsObject::new();
        summary.insert("messageCount".into(), JsValue::Number(items.len() as f64));
        summary.insert("7".into(), "seven".into());
        summary.insert("sessionKey".into(), "from-summary".into());
        summary.insert("absent".into(), JsValue::Undefined);
        summary
    }
}
impl RefreshLog for Fake {
    fn log(&self, message: &JsString) {
        record(&self.0, "log", vec![("message", string(message))]);
    }
    fn warn(&self, message: &str, detail: WarnDetail<'_>) {
        let detail = match detail {
            WarnDetail::Text(text) => string(text),
            WarnDetail::Error(error) => object(vec![
                ("name", "Error".into()),
                ("message", error.message.as_str().into()),
            ]),
        };
        record(
            &self.0,
            "warn",
            vec![("message", message.into()), ("detail", detail)],
        );
    }
}

async fn settle() {
    for _ in 0..50 {
        tokio::task::yield_now().await;
    }
}

fn outcome(outcome: &ReloadOutcome) -> JsValue {
    object(vec![
        ("reloaded", JsValue::Bool(outcome.reloaded)),
        ("changedPaths", strings(&outcome.changed_paths)),
    ])
}

async fn replay(scenario: &JsValue) {
    let id = text(field(scenario, "id").unwrap());
    let options = field(scenario, "options").unwrap();
    let flag = |key: &str| field(options, key).and_then(JsValue::as_bool) == Some(true);
    let calls: Calls = Arc::default();
    let fake = Arc::new(Fake(calls.clone()));
    let store = Arc::new(FakeStore {
        calls: calls.clone(),
        listeners: Mutex::default(),
        next: AtomicUsize::new(0),
        script: Mutex::default(),
        real: Mutex::default(),
    });
    let registry = Arc::new(FakeRegistry {
        calls: calls.clone(),
        pending: Mutex::default(),
        throws: Mutex::default(),
    });
    let router: Option<Arc<dyn RefreshRouter>> = flag("router").then(|| fake.clone() as _);
    let router_calls = calls.clone();
    let bundle = RefreshBundle::new(RefreshOptions {
        config_store: Arc::new(StoreHandle(store.clone())),
        registry: registry.clone(),
        memory_maintenance: fake.clone(),
        router: Arc::new(move || {
            record(&router_calls, "getRouter", vec![]);
            router.clone()
        }),
        project_root: "/work/root".into(),
        memory_diagnostics: flag("diagnostics").then(|| fake.clone() as _),
        sdk: flag("sdk").then(|| fake.clone() as _),
        log: fake.clone(),
    });
    let scratch = flag("realStore").then(|| Scratch::new(&format!("runtime-refresh-{id}")));
    let home = scratch.as_ref().map(|s| s.path().join("home"));
    if let Some(home) = &home {
        std::fs::create_dir(home).unwrap();
    }
    let JsValue::Array(steps) = field(scenario, "steps").unwrap() else {
        panic!()
    };
    let JsValue::Array(expected) = field(scenario, "results").unwrap() else {
        panic!()
    };
    assert_eq!(steps.len(), expected.len(), "{id}: step/result count");
    for (index, (step, expected)) in steps.iter().zip(expected).enumerate() {
        let op = text(field(step, "op").unwrap());
        let number = |key: &str| field(step, key).and_then(JsValue::as_number).unwrap() as usize;
        let mut out: Vec<(&str, JsValue)> = vec![];
        *store.script.lock().unwrap() = Script {
            emits: match field(step, "emits") {
                Some(JsValue::Array(events)) => events
                    .iter()
                    .map(|event| {
                        (
                            js_strings(field(event, "changedPaths").unwrap()),
                            classes(field(event, "changeClasses").unwrap()),
                        )
                    })
                    .collect(),
                _ => vec![],
            },
            fail: field(step, "fail").map(text),
        };
        match op.as_str() {
            "attach" => bundle.attach(),
            "dispose" => bundle.dispose(),
            "bind" => bundle.bind_server(fake.clone()),
            "emit" => {
                let event = field(step, "event").unwrap();
                store.emit(
                    &js_strings(field(event, "changedPaths").unwrap()),
                    &classes(field(event, "changeClasses").unwrap()),
                );
            }
            "flush" => settle().await,
            "resolve" | "reject" => {
                let result = match op.as_str() {
                    "resolve" => Ok(()),
                    _ => Err(RuntimeError::new(text(field(step, "message").unwrap()))),
                };
                let sender = registry.pending.lock().unwrap()[number("index")]
                    .take()
                    .unwrap();
                sender.send(result).unwrap();
            }
            "registryThrows" => {
                *registry.throws.lock().unwrap() = Some(text(field(step, "message").unwrap()))
            }
            "write" => std::fs::write(
                home.as_ref().unwrap().join("pilotdeck.yaml"),
                text(field(step, "yaml").unwrap()),
            )
            .unwrap(),
            "createStore" => {
                let home = home.as_ref().unwrap();
                let env = Env::from([
                    ("HOME".to_owned(), home.to_string_lossy().into_owned()),
                    ("PILOT_HOME".to_owned(), home.to_string_lossy().into_owned()),
                ]);
                let real =
                    ConfigStore::new(StoreOptions::new(home, home, env)).expect("initial load");
                out.push(("version", real.snapshot().get("version").unwrap().clone()));
                *store.real.lock().unwrap() = Some(real);
            }
            "reloadConfig" => match bundle.reload_config().await {
                Ok(result) => out.push(("result", outcome(&result))),
                Err(error) => out.push(("error", error_record(&error))),
            },
            "refreshBeforeTurn" => match bundle.refresh_config_before_turn().await {
                Ok(()) => out.push(("result", JsValue::Undefined)),
                Err(error) => out.push(("error", error_record(&error))),
            },
            "reloadExtensions" => {
                let input = field(step, "input");
                let result = bundle.reload_extensions(ExtensionsReload {
                    project_key: input
                        .and_then(|input| field(input, "projectKey"))
                        .map(|key| key.as_string().unwrap().clone()),
                    changed_paths: input
                        .and_then(|input| field(input, "changedPaths"))
                        .map(js_strings),
                });
                out.push(("result", outcome(&result)));
            }
            "afterTurn" => {
                let input = field(step, "input").unwrap();
                let key =
                    |name: &str| field(input, name).map(|value| value.as_string().unwrap().clone());
                bundle
                    .after_turn_completed(TurnCompleted {
                        session_key: key("sessionKey").unwrap(),
                        project_key: key("projectKey"),
                        run_id: key("runId"),
                    })
                    .unwrap();
                out.push(("result", JsValue::Undefined));
            }
            other => panic!("{id}: unknown op {other}"),
        }
        out.push((
            "calls",
            JsValue::Array(std::mem::take(&mut *calls.lock().unwrap())),
        ));
        assert_eq!(
            encode(&normalize(&object(out))),
            encode(expected),
            "{id}: step {index} ({op})"
        );
    }
    bundle.dispose();
}

#[tokio::test]
async fn refresh_bundle_matches_typescript_call_sequences() {
    let oracle = baseline::oracle(ORACLE);
    let JsValue::Array(scenarios) = field(&oracle, "scenarios").unwrap() else {
        panic!()
    };
    assert_eq!(
        field(&oracle, "count").and_then(JsValue::as_number),
        Some(SCENARIOS as f64)
    );
    assert_eq!(scenarios.len(), SCENARIOS);
    let steps: usize = scenarios
        .iter()
        .map(|s| match field(s, "steps") {
            Some(JsValue::Array(steps)) => steps.len(),
            _ => 0,
        })
        .sum();
    assert_eq!(steps, STEPS);
    let mut ids: Vec<_> = scenarios
        .iter()
        .map(|s| text(field(s, "id").unwrap()))
        .collect();
    ids.sort();
    ids.dedup();
    assert_eq!(ids.len(), SCENARIOS, "duplicate scenario ids");
    for scenario in scenarios {
        replay(scenario).await;
    }
}
