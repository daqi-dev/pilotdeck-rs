// Replay of the real TS DefaultContextRuntime.prepareForModel corpus.
// Fixture: tests/fixtures/context-prepare/. Each case/step compares the whole
// ModelContext serialization byte-for-byte (no key sorting, no tolerance).
#[path = "common/baseline.rs"]
mod baseline;
use base64::Engine as _;
use pilotdeck_gateway::host::Cancellation;
use pilotdeck_gateway::host::modules::context::{ContextInput, ContextRuntime};
use pilotdeck_gateway::runtime::context::{
    ContributedCommand, ContributedPrompt, ContributedSkill, ContributionContext,
    DefaultContextRuntime, ExtensionResolver, InstructionDiscovery, McpServerInstruction, Options,
    PromptAssembler, PromptContributionRegistry, RuntimeContextContribution, RuntimeContextSurface,
    SectionContribution, TextProvider, VariableProvider,
};
use pilotdeck_gateway::runtime::context::{
    cache::PromptCacheCoordinator,
    memory::{
        MemoryDiagnostic, MemoryFuture, MemoryResolver, MemoryRetrieveInput, MemoryRetrieveResult,
    },
};
use pilotdeck_protocol::{JsObject, JsValue, stringify_json};
use std::{
    os::unix::fs::symlink,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};
const ORACLE: &str = include_str!("fixtures/context-prepare/oracle.json");
const CASES: usize = 72;
const STEPS: usize = 138;
static UNDEFINED: JsValue = JsValue::Undefined;

fn field<'a>(value: &'a JsValue, key: &str) -> &'a JsValue {
    value.get(key).unwrap_or(&UNDEFINED)
}
fn object(value: &JsValue) -> &JsObject {
    value.as_object().expect("object")
}
fn array(value: &JsValue) -> &[JsValue] {
    if let JsValue::Array(items) = value {
        items
    } else {
        panic!("array {value:?}")
    }
}
fn text(value: &JsValue) -> String {
    value.as_string().expect("string").to_utf8().expect("utf8")
}
fn optional_text(value: &JsValue) -> Option<String> {
    (!value.is_undefined() && !matches!(value, JsValue::Null)).then(|| text(value))
}
fn number(value: &JsValue) -> f64 {
    value.as_number().expect("number")
}
fn decode_fixture(value: &JsValue, root: &str) -> JsValue {
    // JSON cannot carry NaN/Infinity/-0; the generator records those JS
    // numbers as {"$number": "..."} and the replay rebuilds them here.
    if let JsValue::Object(object) = value
        && object.len() == 1
        && let Some(JsValue::String(label)) = object.get("$number")
    {
        match label.to_utf8().as_deref() {
            Ok("NaN") => return JsValue::Number(f64::NAN),
            Ok("Infinity") => return JsValue::Number(f64::INFINITY),
            Ok("-Infinity") => return JsValue::Number(f64::NEG_INFINITY),
            Ok("-0") => return JsValue::Number(-0.0),
            _ => {}
        }
    }
    // It also cannot carry `undefined` array elements or thousand-level
    // container nesting at a readable oracle size; the generator marks both
    // and the replay rebuilds the exact values here.
    if let JsValue::Object(object) = value
        && object.len() == 1
    {
        if matches!(object.get("$undefined"), Some(JsValue::Bool(true))) {
            return JsValue::Undefined;
        }
        if let Some(JsValue::Number(depth)) = object.get("$deepArray") {
            let mut built = JsValue::Number(0.0);
            for _ in 0..*depth as u64 {
                built = JsValue::Array(vec![built]);
            }
            return built;
        }
    }
    match value {
        JsValue::String(value) => {
            let text = value.to_utf8().expect("utf8 fixture string");
            JsValue::String(text.replace("<fixture>", root).into())
        }
        JsValue::Array(items) => JsValue::Array(
            items
                .iter()
                .map(|item| decode_fixture(item, root))
                .collect(),
        ),
        JsValue::Object(object) => {
            let mut decoded = JsObject::new();
            for (key, value) in object.entries() {
                decoded.insert(key.clone(), decode_fixture(value, root));
            }
            JsValue::Object(decoded)
        }
        value => value.clone(),
    }
}

#[derive(Clone, Default)]
struct FakeExtension {
    commands: Vec<ContributedCommand>,
    skills: Vec<ContributedSkill>,
    mcp: Vec<McpServerInstruction>,
    prompts: Vec<ContributedPrompt>,
}
impl ExtensionResolver for FakeExtension {
    fn list_commands(&self) -> Vec<ContributedCommand> {
        self.commands.clone()
    }
    fn list_skills(&self) -> Vec<ContributedSkill> {
        self.skills.clone()
    }
    fn list_mcp_instructions(&self) -> Vec<McpServerInstruction> {
        self.mcp.clone()
    }
    fn list_prompt_contributions(&self) -> Vec<ContributedPrompt> {
        self.prompts.clone()
    }
}
fn extension(value: &JsValue) -> FakeExtension {
    let Some(value) = value.as_object() else {
        return FakeExtension::default();
    };
    let commands = value
        .get("commands")
        .map(array)
        .unwrap_or(&[])
        .iter()
        .map(|command| ContributedCommand {
            name: text(field(command, "name")),
            description: optional_text(field(command, "description")),
            argument_hint: optional_text(field(command, "argumentHint")),
        })
        .collect();
    let skills = value
        .get("skills")
        .map(array)
        .unwrap_or(&[])
        .iter()
        .map(|skill| ContributedSkill {
            name: text(field(skill, "name")),
            description: optional_text(field(skill, "description")),
            path: text(field(skill, "path")),
        })
        .collect();
    let mcp = value
        .get("mcp")
        .map(array)
        .unwrap_or(&[])
        .iter()
        .map(|server| McpServerInstruction {
            server_name: text(field(server, "serverName")),
            instructions: optional_text(field(server, "instructions")),
        })
        .collect();
    let prompts = value
        .get("promptContributions")
        .map(array)
        .unwrap_or(&[])
        .iter()
        .map(|prompt| ContributedPrompt {
            name: text(field(prompt, "name")),
            content: text(field(prompt, "content")),
        })
        .collect();
    FakeExtension {
        commands,
        skills,
        mcp,
        prompts,
    }
}

fn materialize(oracle: &JsValue, root: &Path) {
    for directory in array(field(oracle, "directories")) {
        std::fs::create_dir_all(root.join(text(directory))).unwrap();
    }
    for file in array(field(oracle, "files")) {
        let path = root.join(text(field(file, "path")));
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(text(field(file, "base64")))
            .unwrap();
        std::fs::write(path, bytes).unwrap();
    }
    for link in array(field(oracle, "symlinks")) {
        let path = root.join(text(field(link, "path")));
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        symlink(text(field(link, "target")), path).unwrap();
    }
}

/// Per-step abort mode (`before` / `section` / `variable`). Providers abort the
/// request signal they receive through `ContributionContext`, so the corpus
/// proves the field is propagated, not just consulted at the entry point.
type AbortMode = Arc<Mutex<Option<String>>>;

fn text_provider(spec: &JsValue, abort_mode: &AbortMode) -> TextProvider {
    let Some(dynamic) = spec.get("dynamic").and_then(JsValue::as_object) else {
        return TextProvider::Static(text(field(spec, "text")));
    };
    let kind = dynamic
        .get("kind")
        .and_then(JsValue::as_string)
        .map(|value| String::from_utf16_lossy(value.units()))
        .expect("dynamic kind");
    let value = dynamic.get("value").cloned().unwrap_or(JsValue::Undefined);
    let message = dynamic
        .get("message")
        .and_then(JsValue::as_string)
        .map(|value| String::from_utf16_lossy(value.units()))
        .unwrap_or_default();
    let when = dynamic
        .get("when")
        .and_then(JsValue::as_string)
        .map(|value| String::from_utf16_lossy(value.units()))
        .unwrap_or_default();
    let abort_mode = abort_mode.clone();
    TextProvider::Dynamic(Arc::new(move |context: &ContributionContext| {
        let kind = kind.clone();
        let value = value.clone();
        let message = message.clone();
        let when = when.clone();
        let abort_mode = abort_mode.clone();
        // The future is `'static`, so the provider must abort synchronously
        // here rather than borrow `context` from inside the async block.
        if kind == "abort-when" && abort_mode.lock().unwrap().as_deref() == Some(when.as_str()) {
            context
                .abort
                .as_ref()
                .expect("provider receives the request abort signal")
                .cancel(None);
        }
        Box::pin(async move {
            match kind.as_str() {
                "throw" => Err(message),
                "non-string" => Ok(value),
                "abort-when" => Ok(JsValue::String(
                    value.as_string().cloned().expect("abort provider value"),
                )),
                _ => Ok(JsValue::String(
                    value
                        .as_string()
                        .cloned()
                        .expect("dynamic string provider value"),
                )),
            }
        })
    }))
}

struct FixtureCompaction(Arc<Mutex<String>>);
impl pilotdeck_gateway::runtime::context::compaction::engine::port::CompactionRunner
    for FixtureCompaction
{
    fn auto_compact(&self, _input: ContextInput) -> pilotdeck_gateway::host::HostFuture<JsValue> {
        let result = self.0.lock().unwrap().clone();
        Box::pin(async move {
            let mut fields = JsObject::new();
            fields.insert("type".into(), JsValue::String(result.into()));
            Ok(JsValue::Object(fields))
        })
    }
}

struct FixtureMemory {
    spec: JsValue,
    abort_mode: AbortMode,
    expected_root: String,
    parent: Arc<Mutex<Option<Cancellation>>>,
    child: Arc<Mutex<Option<Cancellation>>>,
}
impl MemoryResolver for FixtureMemory {
    fn retrieve(&self, input: MemoryRetrieveInput) -> MemoryFuture {
        assert!(!input.signal.is_cancelled(), "fresh forwarded signal");
        *self.child.lock().unwrap() = Some(input.signal.clone());
        assert_eq!(input.session_id, "session");
        assert_eq!(input.project_root, self.expected_root);
        if let Some(query) = self.spec.get("expectQuery") {
            assert_eq!(input.query, text(query));
        }
        if let Some(count) = self.spec.get("expectRecentCount") {
            assert_eq!(input.recent_messages.len(), number(count) as usize);
        }
        if field(&self.spec, "abort").as_bool() == Some(true)
            && matches!(
                self.abort_mode.lock().unwrap().as_deref(),
                Some("memory" | "memory-pending")
            )
        {
            // The fixture receives the child signal. The request parent is
            // aborted below via a separate capability shared with the runner.
            self.parent.lock().unwrap().as_ref().unwrap().cancel(None);
        }
        let pending_abort = self.abort_mode.lock().unwrap().as_deref() == Some("memory-pending");
        let spec = self.spec.clone();
        Box::pin(async move {
            if pending_abort || field(&spec, "pending").as_bool() == Some(true) {
                std::future::pending::<()>().await;
            }
            if let Some(error) = spec.get("error") {
                return Err(text(error));
            }
            Ok(MemoryRetrieveResult {
                system_context: spec.get("context").map(text),
                diagnostics: spec
                    .get("diagnostics")
                    .map(array)
                    .unwrap_or(&[])
                    .iter()
                    .map(|d| MemoryDiagnostic {
                        code: text(field(d, "code")),
                        severity: text(field(d, "severity")),
                        message: text(field(d, "message")),
                    })
                    .collect(),
            })
        })
    }
}

#[tokio::test]
async fn prepare_for_model_matches_real_ts() {
    let oracle = baseline::oracle(ORACLE);
    let root = std::env::temp_dir().join(format!("pdrs-context-prepare-{}", uuid::Uuid::new_v4()));
    assert!(!root.exists(), "exclusive fixture directory");
    std::fs::create_dir(&root).unwrap();
    let root_string = root.to_str().unwrap().to_owned();
    materialize(&oracle, &root);
    let environment = field(&oracle, "environment");
    let platform = text(field(environment, "platform"));
    let node_version = text(field(environment, "node"));

    let cases = array(field(&oracle, "cases"));
    assert_eq!(cases.len(), CASES, "case count");
    let mut observed_steps = 0;
    for case in cases {
        let name = text(field(case, "name"));
        let setup = decode_fixture(field(case, "setup"), &root_string);
        let setup = object(&setup);
        let extension = extension(&setup.get("extension").cloned().unwrap_or(JsValue::Null));
        let include_extensions = setup
            .get("includeExtensions")
            .and_then(JsValue::as_bool)
            .unwrap_or(false);
        let surface = match setup.get("surface").and_then(JsValue::as_string) {
            Some(value) if value.equals_str("user_message") => RuntimeContextSurface::UserMessage,
            Some(value) if value.equals_str("system_prompt") => RuntimeContextSurface::SystemPrompt,
            Some(_) => RuntimeContextSurface::Other,
            None => RuntimeContextSurface::SystemPrompt,
        };
        let discovery = setup
            .get("discovery")
            .and_then(JsValue::as_object)
            .map(|value| {
                let text_field = |key: &str| {
                    value
                        .get(key)
                        .and_then(JsValue::as_string)
                        .map(|value| String::from_utf16_lossy(value.units()))
                        .unwrap_or_default()
                };
                let managed = value
                    .get("managed")
                    .and_then(JsValue::as_string)
                    .filter(|value| !value.is_empty())
                    .map(|value| PathBuf::from(String::from_utf16_lossy(value.units())));
                InstructionDiscovery::new(
                    PathBuf::from(text_field("projectRoot")),
                    PathBuf::from(text_field("cwd")),
                    PathBuf::from(text_field("pilotHome")),
                    managed,
                )
            });
        let abort_mode: AbortMode = Arc::new(Mutex::new(None));
        let prompt_contributions =
            setup
                .get("contributions")
                .and_then(JsValue::as_object)
                .map(|contributions| {
                    let registry = PromptContributionRegistry::new(Some("fixture"));
                    if setup
                        .get("extension")
                        .and_then(JsValue::as_object)
                        .and_then(|extension| extension.get("promptContributions"))
                        .is_some()
                    {
                        let assembler = PromptAssembler::with_environment(
                            Arc::new(extension.clone()),
                            "en-US".parse().unwrap(),
                            platform.clone(),
                            node_version.clone(),
                        );
                        assembler
                            .register_extension_prompt_contributions(&registry)
                            .unwrap();
                    }
                    for section in contributions.get("sections").map(array).unwrap_or(&[]) {
                        registry
                            .register_section(SectionContribution {
                                name: text(field(section, "name")),
                                order: number(field(section, "order")),
                                text: text_provider(section, &abort_mode),
                                complete: field(section, "complete").as_bool().unwrap_or(false),
                            })
                            .unwrap();
                    }
                    for context in contributions
                        .get("runtimeContexts")
                        .map(array)
                        .unwrap_or(&[])
                    {
                        registry
                            .register_runtime_context(RuntimeContextContribution {
                                name: text(field(context, "name")),
                                order: number(field(context, "order")),
                                text: text_provider(context, &abort_mode),
                            })
                            .unwrap();
                    }
                    for variable in contributions.get("variables").map(array).unwrap_or(&[]) {
                        let value = text(field(variable, "value"));
                        let abort = field(variable, "abort").as_bool().unwrap_or(false);
                        let provider: VariableProvider =
                            Arc::new(move |context: &ContributionContext| {
                                let value = value.clone();
                                if abort {
                                    context
                                        .abort
                                        .as_ref()
                                        .expect(
                                            "variable provider receives the request abort signal",
                                        )
                                        .cancel(None);
                                }
                                Box::pin(async move { Ok(Some(value)) })
                            });
                        registry
                            .register_variable(&text(field(variable, "name")), provider)
                            .unwrap();
                    }
                    registry
                });
        let memory_parent = Arc::new(Mutex::new(None));
        let memory_child = Arc::new(Mutex::new(None));
        let memory_resolver = setup.get("memory").map(|spec| {
            Arc::new(FixtureMemory {
                spec: spec.clone(),
                abort_mode: abort_mode.clone(),
                expected_root: spec
                    .get("expectProjectRoot")
                    .or_else(|| setup.get("projectRoot"))
                    .map(text)
                    .unwrap_or_else(|| {
                        if name.starts_with("cache-") {
                            "/pdrs-cache-fixture".into()
                        } else {
                            format!("{root_string}/project")
                        }
                    }),
                parent: memory_parent.clone(),
                child: memory_child.clone(),
            }) as Arc<dyn MemoryResolver>
        });
        let coordinator = Arc::new(PromptCacheCoordinator::new("en-US".parse().unwrap()));
        let clock = Arc::new(Mutex::new(0.0));
        let now = {
            let clock = clock.clone();
            Arc::new(move || *clock.lock().unwrap())
        };
        let compact_result = Arc::new(Mutex::new("skipped".to_owned()));
        let runtime = DefaultContextRuntime::new(Options {
            compaction: Some(Arc::new(
                pilotdeck_gateway::runtime::context::compaction::engine::port::CompactionPort {
                    auto_compact: Some(Arc::new(FixtureCompaction(compact_result.clone()))),
                    ..Default::default()
                },
            )),
            memory_resolver,
            project_root: setup.get("projectRoot").map(text),
            memory_retrieval_timeout_ms: setup
                .get("memoryTimeoutMs")
                .map(number)
                .unwrap_or(30_000.0),
            prompt_cache_coordinator: Some(coordinator.clone()),
            extension: Some(Arc::new(extension)),
            include_extensions_with_custom_system_prompt: include_extensions,
            runtime_context_surface: surface,
            instruction_discovery: discovery,
            prompt_contributions,
            tool_result_budget: None,
            now: Some(now),
            platform: Some(platform.clone()),
            node_version: Some(node_version.clone()),
            locale: Some("en-US".into()),
            ..Options::default()
        });
        for (index, step) in array(field(case, "steps")).iter().enumerate() {
            *clock.lock().unwrap() = number(field(step, "clockMs"));
            let input = decode_fixture(field(step, "input"), &root_string);
            let fields = input.as_object().expect("input object").clone();
            let abort = step
                .get("abort")
                .and_then(JsValue::as_string)
                .map(|value| String::from_utf16_lossy(value.units()));
            *abort_mode.lock().unwrap() = abort.clone();
            let abort_signal = abort.as_ref().map(|mode| {
                let signal = Cancellation::default();
                if mode == "before" {
                    signal.cancel(None);
                }
                signal
            });
            *memory_parent.lock().unwrap() = abort_signal.clone();
            *memory_child.lock().unwrap() = None;
            if let Some(compact) = step.get("compact") {
                *compact_result.lock().unwrap() = text(compact);
                runtime
                    .try_auto_compact(ContextInput {
                        fields: fields.clone(),
                        abort_signal: None,
                        budget_evaluator: None,
                    })
                    .await
                    .unwrap();
            }
            if let Some(action) = step.get("cacheAction") {
                let session = fields.get("sessionId").unwrap().as_string().unwrap();
                match text(action).as_str() {
                    "reset" => coordinator.reset(session),
                    "dispose" => runtime.dispose(),
                    "release" => runtime.release_session(session),
                    other => panic!("unknown cache action {other}"),
                }
            }
            let result = runtime
                .prepare_for_model(ContextInput {
                    fields,
                    abort_signal: abort_signal
                        .map(|signal| Arc::new(signal) as Arc<dyn std::any::Any + Send + Sync>),
                    budget_evaluator: None,
                })
                .await;
            // A step that expects an error can fail before the memory stage
            // (malformed text block), so the provider is legitimately uncalled.
            if let Some(memory) = setup.get("memory")
                && step.get("error").is_none()
            {
                let child = memory_child.lock().unwrap();
                if abort.as_deref() == Some("before") {
                    assert!(child.is_none(), "pre-abort skips provider");
                } else if let Some(child) = child.as_ref() {
                    let expected_cancelled = field(memory, "pending").as_bool() == Some(true)
                        || matches!(abort.as_deref(), Some("memory" | "memory-pending"));
                    assert_eq!(
                        child.is_cancelled(),
                        expected_cancelled,
                        "{name} child cancellation"
                    );
                } else {
                    panic!("{name}: provider was not called");
                }
            }
            if let Some(expected_error) = step.get("error") {
                match result {
                    Ok(_) => panic!("case {name} step {index}: expected an error"),
                    Err(error) => assert_eq!(
                        error.message,
                        text(expected_error),
                        "case {name} step {index} error"
                    ),
                }
                observed_steps += 1;
                continue;
            }
            let result = match result {
                Ok(result) => result,
                Err(error) => panic!("case {name} step {index}: {}", error.message),
            };
            let actual = stringify_json(&result)
                .unwrap()
                .unwrap()
                .replace(&root_string, "<fixture>");
            let expected = text(field(step, "output"));
            if actual != expected {
                let at = actual
                    .bytes()
                    .zip(expected.bytes())
                    .position(|(left, right)| left != right)
                    .unwrap_or_else(|| actual.len().min(expected.len()));
                let start = at.saturating_sub(80);
                let end = (at + 120).min(actual.len()).min(expected.len());
                panic!(
                    "case {name} step {index} differs at byte {at}\nactual: {:?}\nexpected: {:?}",
                    String::from_utf8_lossy(&actual.as_bytes()[start..end]),
                    String::from_utf8_lossy(&expected.as_bytes()[start..end])
                );
            }
            observed_steps += 1;
        }
    }
    assert_eq!(observed_steps, STEPS, "step count");
}
