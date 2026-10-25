//! Real TS `DefaultContextRuntime.captureTurn`, `canonicalMessagesToMemoryMessages`
//! and context manifest advertisement replay (cc7c4e6).
//!
//! Fixture: `tests/fixtures/edgeclaw-capture-turn/`. The generator runs the
//! pinned TS runtime with a recording / throwing / rejecting resolver; the Rust
//! replay uses the same scenario table from `oracle.json`.
#[path = "common/baseline.rs"]
mod baseline;
#[path = "common/scratch.rs"]
mod scratch;
use pilotdeck_gateway::{
    host::{
        HostError,
        modules::{
            CompositionSwitches, ModuleRouter,
            context::{ContextInput, ContextRuntime, Handler as ContextHandler, Operation},
            manifest::{ManifestInput, PermissionSnapshot, TurnCallbacks, compose_manifest},
            tool_context::{
                ContextRequestIdentity, HostToolCheckpoint, ToolContextPorts, TurnBinding,
                TurnEnvironment,
            },
        },
    },
    runtime::context::{
        DefaultContextRuntime, Options,
        memory::{
            CanonicalMessagesToMemoryMessagesOptions, ContextMemoryMessage, MemoryCaptureFuture,
            MemoryCaptureTurnInput, MemoryFuture, MemoryResolver, MemoryRetrieveInput,
            MemoryRetrieveResult, canonical_messages_to_memory_messages,
        },
    },
};
use pilotdeck_protocol::{JsObject, JsString, JsValue, stringify_json};
use scratch::Scratch;
use std::{
    fs,
    path::Path,
    process::{Command, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};
const ORACLE: &str = include_str!("fixtures/edgeclaw-capture-turn/oracle.json");
static UNDEFINED: JsValue = JsValue::Undefined;
#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    NoResolver,
    Recording,
    Throwing,
    Rejecting,
}
struct Resolver {
    mode: Mode,
    calls: Arc<Mutex<Vec<JsValue>>>,
    invoked: AtomicBool,
}
impl Resolver {
    fn new(mode: Mode) -> Self {
        Self {
            mode,
            calls: Arc::new(Mutex::new(Vec::new())),
            invoked: AtomicBool::new(false),
        }
    }
    fn calls(&self) -> Vec<JsValue> {
        self.calls.lock().unwrap().clone()
    }
    fn invoked(&self) -> bool {
        self.invoked.load(Ordering::SeqCst)
    }
}
impl MemoryResolver for Resolver {
    fn retrieve(&self, _: MemoryRetrieveInput) -> MemoryFuture {
        Box::pin(async { Ok(MemoryRetrieveResult::default()) })
    }
    fn capture_turn(&self, input: MemoryCaptureTurnInput) -> MemoryCaptureFuture {
        self.invoked.store(true, Ordering::SeqCst);
        match self.mode {
            Mode::Throwing => return Box::pin(async { Err("capture boom".into()) }),
            Mode::Rejecting => return Box::pin(async { Err("capture reject".into()) }),
            Mode::NoResolver | Mode::Recording => {}
        }
        let messages = canonical_messages_to_memory_messages(
            &input.messages,
            CanonicalMessagesToMemoryMessagesOptions {
                include_fork_carryover: Some(false),
            },
        );
        let calls = self.calls.clone();
        Box::pin(async move {
            let messages = messages?;
            let mut call = JsObject::new();
            call.insert("sessionId".into(), JsValue::String(input.session_id.into()));
            call.insert(
                "projectRoot".into(),
                JsValue::String(input.project_root.into()),
            );
            call.insert("errored".into(), JsValue::Bool(input.errored));
            call.insert(
                "messages".into(),
                JsValue::Array(messages.iter().map(ContextMemoryMessage::to_js).collect()),
            );
            calls.lock().unwrap().push(JsValue::Object(call));
            Ok(())
        })
    }
}
fn field<'a>(value: &'a JsValue, key: &str) -> &'a JsValue {
    value.get(key).unwrap_or(&UNDEFINED)
}
fn array(value: &JsValue) -> &[JsValue] {
    if let JsValue::Array(items) = value {
        items
    } else {
        panic!("expected array: {value:?}")
    }
}
fn text(value: &JsValue) -> String {
    value.as_string().expect("string").to_utf8().expect("UTF-8")
}
fn obj(items: impl IntoIterator<Item = (&'static str, JsValue)>) -> JsValue {
    let mut object = JsObject::new();
    for (key, value) in items {
        object.insert(key.into(), value);
    }
    JsValue::Object(object)
}
fn same(actual: &JsValue, expected: &JsValue, name: &str) {
    let actual = stringify_json(actual).unwrap().unwrap();
    let expected = stringify_json(expected).unwrap().unwrap();
    assert_eq!(actual, expected, "{name}");
}
fn mode(name: &str) -> Mode {
    match name {
        "none" => Mode::NoResolver,
        "recording" => Mode::Recording,
        "throwing" => Mode::Throwing,
        "rejecting" => Mode::Rejecting,
        other => panic!("unknown resolver mode {other}"),
    }
}
fn input_fields(spec: &JsValue) -> JsObject {
    let mut fields = JsObject::new();
    fields.insert("sessionId".into(), field(spec, "sessionId").clone());
    fields.insert("turnId".into(), JsValue::from("turn"));
    fields.insert("messages".into(), field(spec, "messages").clone());
    fields.insert("errored".into(), field(spec, "errored").clone());
    fields
}
async fn run_scenario(spec: &JsValue) -> (JsValue, Arc<Resolver>) {
    let mode = mode(&text(field(spec, "resolver")));
    let resolver = Arc::new(Resolver::new(mode));
    let mut options = Options::default();
    if mode != Mode::NoResolver {
        options.memory_resolver = Some(resolver.clone());
    }
    if let Some(root) = spec.get("projectRoot") {
        options.project_root = Some(text(root));
    }
    let runtime = DefaultContextRuntime::new(options);
    let result = runtime
        .capture_turn(ContextInput {
            fields: input_fields(spec),
            abort_signal: None,
            budget_evaluator: None,
        })
        .await;
    assert!(result.is_ok(), "captureTurn must never reject");
    let throws = match mode {
        Mode::Throwing => JsValue::from("capture boom"),
        _ => JsValue::Null,
    };
    let rejects = match mode {
        Mode::Rejecting => JsValue::from("capture reject"),
        _ => JsValue::Null,
    };
    (
        obj([
            ("name", field(spec, "name").clone()),
            (
                "supportsCaptureTurn",
                JsValue::Bool(runtime.supports(Operation::CaptureTurn)),
            ),
            ("invoked", JsValue::Bool(resolver.invoked())),
            ("calls", JsValue::Array(resolver.calls())),
            ("throws", throws),
            ("rejects", rejects),
            ("runnerThrew", JsValue::Null),
        ]),
        resolver,
    )
}
fn conversion_options(spec: &JsValue) -> CanonicalMessagesToMemoryMessagesOptions {
    CanonicalMessagesToMemoryMessagesOptions {
        include_fork_carryover: spec.get("includeForkCarryover").and_then(JsValue::as_bool),
    }
}
async fn observations() -> JsValue {
    let oracle = baseline::oracle(ORACLE);
    let mut scenarios = Vec::new();
    for spec in array(field(&oracle, "scenarioSpecs")) {
        scenarios.push(run_scenario(spec).await.0);
    }
    let mut conversions = Vec::new();
    for spec in array(field(&oracle, "conversionSpecs")) {
        let messages = canonical_messages_to_memory_messages(
            array(field(spec, "messages")),
            conversion_options(spec),
        )
        .expect("canonical fixture messages convert");
        conversions.push(obj([
            ("name", field(spec, "name").clone()),
            (
                "includeForkCarryover",
                spec.get("includeForkCarryover")
                    .and_then(JsValue::as_bool)
                    .map(JsValue::Bool)
                    .unwrap_or(JsValue::Null),
            ),
            (
                "result",
                JsValue::Array(messages.iter().map(ContextMemoryMessage::to_js).collect()),
            ),
        ]));
    }
    obj([
        ("scenarios", JsValue::Array(scenarios)),
        ("conversions", JsValue::Array(conversions)),
        ("manifest", manifest_observation().await),
    ])
}
async fn manifest_observation() -> JsValue {
    let resolver = Arc::new(Resolver::new(Mode::Recording));
    let runtime = Arc::new(DefaultContextRuntime::new(Options {
        memory_resolver: Some(resolver),
        project_root: Some("/pdrs-fixture/manifest".into()),
        ..Options::default()
    }));
    let identity = ContextRequestIdentity::new(
        TurnBinding {
            config: JsObject::new(),
            input: obj([
                ("sessionId", JsValue::from("session")),
                ("turnId", JsValue::from("turn")),
            ])
            .as_object()
            .unwrap()
            .clone(),
            checkpoint: HostToolCheckpoint::default(),
            abort_signal: None,
            environment: Arc::new(IdentityEnvironment),
        },
        ToolContextPorts::default(),
    );
    let context = ContextHandler::new(runtime.clone(), identity, JsObject::new(), None);
    assert!(context.methods().contains(&"capture_turn"));
    let router = ModuleRouter {
        context,
        ..ModuleRouter::default()
    };
    let inventory = router.module_inventory(&CompositionSwitches::default());
    let manifest = compose_manifest(&ManifestInput {
        tools: Vec::new(),
        permission: PermissionSnapshot {
            cwd: JsValue::from("/pdrs-fixture/project"),
            permission_mode: JsValue::from("default"),
            permission_context: JsObject::new(),
            turn_can_prompt: None,
        },
        modules: inventory,
        turn: TurnCallbacks {
            drain_steer_messages: false,
            drain_or_close_steer_mailbox: false,
            on_compact_persisted: false,
        },
    });
    let host_modules = manifest.get("hostModules").expect("hostModules");
    obj([
        (
            "captureTurnType",
            if runtime.supports(Operation::CaptureTurn) {
                JsValue::from("function")
            } else {
                JsValue::from("undefined")
            },
        ),
        ("hostModulesContext", field(host_modules, "context").clone()),
    ])
}
struct IdentityEnvironment;
impl TurnEnvironment for IdentityEnvironment {
    fn build(
        &self,
        _: Option<&JsValue>,
        _: &JsString,
        _: &JsString,
        _: &JsString,
    ) -> Result<JsObject, HostError> {
        panic!("context identity must not build a tool environment")
    }
}
#[tokio::test]
async fn ts_capture_turn_scenarios_match() {
    let oracle = baseline::oracle(ORACLE);
    let mut actual = Vec::new();
    for spec in array(field(&oracle, "scenarioSpecs")) {
        actual.push(run_scenario(spec).await.0);
    }
    same(
        &JsValue::Array(actual),
        field(&oracle, "scenarios"),
        "captureTurn scenarios",
    );
}
#[tokio::test]
async fn ts_capture_turn_conversions_match() {
    let oracle = baseline::oracle(ORACLE);
    let mut actual = Vec::new();
    for spec in array(field(&oracle, "conversionSpecs")) {
        let messages = canonical_messages_to_memory_messages(
            array(field(spec, "messages")),
            conversion_options(spec),
        )
        .expect("canonical fixture messages convert");
        actual.push(obj([
            ("name", field(spec, "name").clone()),
            (
                "includeForkCarryover",
                spec.get("includeForkCarryover")
                    .and_then(JsValue::as_bool)
                    .map(JsValue::Bool)
                    .unwrap_or(JsValue::Null),
            ),
            (
                "result",
                JsValue::Array(messages.iter().map(ContextMemoryMessage::to_js).collect()),
            ),
        ]));
    }
    same(
        &JsValue::Array(actual),
        field(&oracle, "conversions"),
        "canonicalMessagesToMemoryMessages",
    );
}
#[tokio::test]
async fn resolver_failures_are_swallowed() {
    let oracle = baseline::oracle(ORACLE);
    for spec in array(field(&oracle, "scenarioSpecs")) {
        let name = text(field(spec, "name"));
        if name != "throwing-resolver" && name != "rejecting-resolver" {
            continue;
        }
        let (actual, resolver) = run_scenario(spec).await;
        assert!(resolver.invoked());
        assert_eq!(field(&actual, "runnerThrew"), &JsValue::Null, "{name}");
    }
}
#[tokio::test]
async fn malformed_session_id_matches_ts_startswith_failure() {
    for (value, expected) in [
        (JsValue::Undefined, "reading 'startsWith'"),
        (JsValue::Null, "reading 'startsWith'"),
        (JsValue::Number(1.0), "startsWith is not a function"),
    ] {
        let resolver = Arc::new(Resolver::new(Mode::Recording));
        let runtime = DefaultContextRuntime::new(Options {
            memory_resolver: Some(resolver.clone()),
            ..Options::default()
        });
        let mut fields = JsObject::new();
        fields.insert("sessionId".into(), value);
        fields.insert(
            "messages".into(),
            JsValue::Array(vec![obj([
                ("role", JsValue::from("user")),
                ("content", JsValue::Array(vec![JsValue::from("hi")])),
            ])]),
        );
        let error = runtime
            .capture_turn(ContextInput {
                fields,
                abort_signal: None,
                budget_evaluator: None,
            })
            .await
            .expect_err("non-string sessionId must reject like isAlwaysOnSession")
            .to_string();
        assert!(error.contains(expected), "{error}");
        assert!(!resolver.invoked());
    }
}
#[tokio::test]
async fn always_on_sessions_are_skipped() {
    let oracle = baseline::oracle(ORACLE);
    for spec in array(field(&oracle, "scenarioSpecs")) {
        let name = text(field(spec, "name"));
        if !name.starts_with("always-on-") || name == "always-on-prefix-without-colon" {
            continue;
        }
        let (_, resolver) = run_scenario(spec).await;
        assert!(!resolver.invoked(), "{name} must not reach the resolver");
    }
}
#[tokio::test]
async fn manifest_advertises_capture_turn() {
    let oracle = baseline::oracle(ORACLE);
    same(
        &manifest_observation().await,
        field(&oracle, "manifest"),
        "manifest context methods",
    );
}
#[test]
#[ignore = "requires PDRS_BASELINE (PilotDeck cc7c4e6 with node_modules)"]
fn real_ts_reads_rust_capture_observations() {
    let scratch = Scratch::new("edgeclaw-capture-turn-verify");
    let observed = tokio::runtime::Runtime::new()
        .unwrap()
        .block_on(observations());
    let path = scratch.path().join("rust-observations.json");
    fs::write(
        &path,
        format!("{}\n", stringify_json(&observed).unwrap().unwrap()),
    )
    .unwrap();
    let baseline = std::env::var_os("PDRS_BASELINE").expect("PDRS_BASELINE");
    let mut command = Command::new("node");
    command
        .arg("--disable-warning=ExperimentalWarning")
        .arg("--import")
        .arg(Path::new(&baseline).join("node_modules/tsx/dist/loader.mjs"))
        .arg(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures/edgeclaw-capture-turn/verify.mts"),
        )
        .arg(&path)
        .env("PDRS_BASELINE", &baseline)
        .env("LANG", "en_US.UTF-8")
        .env_remove("LC_ALL")
        .stdin(Stdio::null());
    run_node(&mut command, scratch.path());
}
#[test]
#[ignore = "requires PDRS_BASELINE (PilotDeck cc7c4e6 with node_modules)"]
fn fixture_regenerates_identically_from_ts() {
    baseline::regenerates_identically("edgeclaw-capture-turn");
}
fn run_node(command: &mut Command, scratch: &Path) {
    let stdout = scratch.join("node.stdout");
    let stderr = scratch.join("node.stderr");
    let mut child = command
        .stdout(Stdio::from(fs::File::create(&stdout).unwrap()))
        .stderr(Stdio::from(fs::File::create(&stderr).unwrap()))
        .spawn()
        .unwrap();
    let start = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if start.elapsed() > Duration::from_secs(90) {
            let _ = child.kill();
            panic!("verify.mts timed out");
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    let stdout = fs::read_to_string(&stdout).unwrap_or_default();
    let stderr = fs::read_to_string(&stderr).unwrap_or_default();
    assert!(status.success(), "verify.mts failed: {stdout}\n{stderr}");
    // Keep the real TS replay line in `--include-ignored --nocapture` logs.
    print!("{stdout}");
}
