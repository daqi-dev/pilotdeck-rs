//! Router decision replay against the fixed TypeScript baseline
//! (`tests/fixtures/router-decision/generate.mts`). Expectations, judge prompts
//! and events are all recorded from the real `createRouterRuntime`.
#[path = "common/baseline.rs"]
mod baseline;
use pilotdeck_gateway::runtime::model::router::{
    AbortFlag, CustomRouter, CustomRouterRegistry, NoopCustomRouters, RouterConfig,
    RouterDecisionInput, RouterDeps, RouterFuture, RouterInvokeError, RouterModelInvocation,
    RouterModelRef, RouterRuntime, RouterRuntimeError,
};
use pilotdeck_gateway::runtime::model::stats::TokenStatsCollector;
use pilotdeck_protocol::{JsObject, JsString, JsValue};
use std::{
    collections::HashMap,
    fs,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
};

const FIXTURES: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/router-decision"
);
static UNDEFINED: JsValue = JsValue::Undefined;

fn oracle() -> JsValue {
    let text = fs::read_to_string(format!("{FIXTURES}/oracle.json")).expect("oracle.json");
    assert!(
        !text.contains(".generate-"),
        "oracle.json leaks a generation path"
    );
    baseline::oracle(&text)
}

fn text(value: &JsValue) -> String {
    value
        .as_string()
        .expect("fixture string")
        .to_utf8()
        .expect("fixture UTF-8")
}
fn field<'a>(value: &'a JsValue, key: &str) -> &'a JsValue {
    value.get(key).unwrap_or(&UNDEFINED)
}
fn array(value: &JsValue) -> &[JsValue] {
    match value {
        JsValue::Array(items) => items,
        _ => &[],
    }
}

/// `JSON.stringify` byte identity: property order, number formatting and
/// undefined-dropping rules are all observable in the recorded transcript.
fn wire(value: &JsValue) -> String {
    pilotdeck_protocol::stringify_json(value)
        .ok()
        .flatten()
        .unwrap_or_default()
}

#[derive(Clone)]
enum JudgeStep {
    Text(String),
    Empty,
    Hang,
    Provider {
        code: String,
        message: String,
        retryable: bool,
    },
    Request {
        code: String,
        message: String,
    },
}

struct FakeInvoker {
    capabilities: HashMap<String, f64>,
    multimodal: HashMap<String, Vec<JsString>>,
    script: Vec<JudgeStep>,
    calls: Mutex<Vec<String>>,
    index: Mutex<usize>,
}
impl FakeInvoker {
    fn from_case(case: &JsValue) -> Self {
        let capabilities = field(case, "capabilities")
            .as_object()
            .map(|object| {
                object
                    .entries()
                    .iter()
                    .filter_map(|(key, value)| {
                        Some((String::from_utf16_lossy(key.units()), value.as_number()?))
                    })
                    .collect()
            })
            .unwrap_or_default();
        let multimodal = field(case, "multimodal")
            .as_object()
            .map(|object| {
                object
                    .entries()
                    .iter()
                    .map(|(key, value)| {
                        (
                            String::from_utf16_lossy(key.units()),
                            array(value)
                                .iter()
                                .filter_map(JsValue::as_string)
                                .cloned()
                                .collect(),
                        )
                    })
                    .collect()
            })
            .unwrap_or_default();
        let script = array(field(case, "judgeScript"))
            .iter()
            .map(|step| {
                if field(step, "hang").as_bool() == Some(true) {
                    return JudgeStep::Hang;
                }
                if field(step, "empty").as_bool() == Some(true) {
                    return JudgeStep::Empty;
                }
                if let Some(error) = field(step, "error").as_object() {
                    let kind = field(&JsValue::Object(error.clone()), "kind")
                        .as_string()
                        .map(|value| value.to_utf8().unwrap_or_default())
                        .unwrap_or_default();
                    let code = field(&JsValue::Object(error.clone()), "code")
                        .as_string()
                        .map(|value| value.to_utf8().unwrap_or_default())
                        .unwrap_or_default();
                    let message = field(&JsValue::Object(error.clone()), "message")
                        .as_string()
                        .map(|value| value.to_utf8().unwrap_or_default())
                        .unwrap_or_default();
                    if kind == "provider" {
                        return JudgeStep::Provider {
                            code,
                            message,
                            retryable: field(&JsValue::Object(error.clone()), "retryable")
                                .as_bool()
                                .unwrap_or(false),
                        };
                    }
                    return JudgeStep::Request { code, message };
                }
                JudgeStep::Text(text(field(step, "text")))
            })
            .collect();
        Self {
            capabilities,
            multimodal,
            script,
            calls: Mutex::new(Vec::new()),
            index: Mutex::new(0),
        }
    }
    fn step(&self) -> JudgeStep {
        let mut index = self.index.lock().expect("judge index");
        let step = self
            .script
            .get((*index).min(self.script.len().saturating_sub(1)))
            .cloned()
            .unwrap_or(JudgeStep::Text("<tier>simple</tier>".into()));
        *index += 1;
        step
    }
}
impl RouterModelInvocation for FakeInvoker {
    fn get_capabilities(
        &self,
        provider: &JsString,
        model: &JsString,
    ) -> Result<JsValue, pilotdeck_gateway::host::ModuleError> {
        let key = format!(
            "{}/{}",
            provider.to_utf8().unwrap_or_default(),
            model.to_utf8().unwrap_or_default()
        );
        let Some(max_output_tokens) = self.capabilities.get(&key) else {
            return Err(pilotdeck_gateway::host::ModuleError::new(format!(
                "Unknown provider/model {key}"
            )));
        };
        let mut value = JsObject::new();
        value.insert(
            "maxOutputTokens".into(),
            JsValue::Number(*max_output_tokens),
        );
        value.insert("maxContextTokens".into(), JsValue::Number(200_000.0));
        Ok(JsValue::Object(value))
    }
    fn get_multimodal(
        &self,
        provider: &JsString,
        model: &JsString,
    ) -> Result<JsValue, pilotdeck_gateway::host::ModuleError> {
        let key = format!(
            "{}/{}",
            provider.to_utf8().unwrap_or_default(),
            model.to_utf8().unwrap_or_default()
        );
        let input = self.multimodal.get(&key).cloned().unwrap_or_else(|| {
            ["text", "image", "pdf", "audio"]
                .into_iter()
                .map(JsString::from)
                .collect()
        });
        let mut value = JsObject::new();
        value.insert(
            "input".into(),
            JsValue::Array(input.into_iter().map(JsValue::String).collect()),
        );
        Ok(JsValue::Object(value))
    }
    fn supports_complete(&self) -> bool {
        true
    }
    fn complete(
        &self,
        request: JsValue,
        _abort: Option<pilotdeck_gateway::runtime::model::router::AbortFlag>,
    ) -> RouterFuture<JsValue> {
        let prompt = array(field(&request, "messages"))
            .first()
            .map(|message| {
                array(field(message, "content"))
                    .iter()
                    .find(|block| {
                        field(block, "type")
                            .as_string()
                            .is_some_and(|kind| kind.equals_str("text"))
                    })
                    .map(|block| text(field(block, "text")))
                    .unwrap_or_default()
            })
            .unwrap_or_default();
        self.calls.lock().expect("judge calls").push(prompt);
        match self.step() {
            JudgeStep::Text(value) => {
                let mut block = JsObject::new();
                block.insert("type".into(), "text".into());
                block.insert("text".into(), JsValue::String(value.into()));
                let mut response = JsObject::new();
                response.insert(
                    "content".into(),
                    JsValue::Array(vec![JsValue::Object(block)]),
                );
                response.insert("finishReason".into(), "stop".into());
                Box::pin(async move { Ok(JsValue::Object(response)) })
            }
            JudgeStep::Empty => Box::pin(async move {
                let mut response = JsObject::new();
                response.insert("content".into(), JsValue::Array(Vec::new()));
                response.insert("finishReason".into(), "stop".into());
                Ok(JsValue::Object(response))
            }),
            JudgeStep::Hang => Box::pin(async {
                std::future::pending::<Result<JsValue, RouterInvokeError>>().await
            }),
            JudgeStep::Provider {
                code,
                message,
                retryable,
            } => Box::pin(async move {
                Err(RouterInvokeError::provider(
                    code.as_str(),
                    message.as_str(),
                    retryable,
                ))
            }),
            JudgeStep::Request { code, message } => {
                Box::pin(
                    async move { Err(RouterInvokeError::request(code.as_str(), message.as_str())) },
                )
            }
        }
    }
}

struct FakeCustom {
    provider: JsString,
    model: JsString,
    error: Option<String>,
}
impl CustomRouter for FakeCustom {
    fn decide(&self, _input: JsValue) -> RouterFuture<JsValue> {
        if let Some(error) = self.error.clone() {
            return Box::pin(async move { Err(RouterInvokeError::other(error)) });
        }
        let mut decision = JsObject::new();
        decision.insert("provider".into(), JsValue::String(self.provider.clone()));
        decision.insert("model".into(), JsValue::String(self.model.clone()));
        Box::pin(async move { Ok(JsValue::Object(decision)) })
    }
}
struct FakeRegistry(Arc<FakeCustom>);
impl CustomRouterRegistry for FakeRegistry {
    fn lookup_router(
        &self,
        _extension_id: &JsString,
        _session_id: &JsString,
    ) -> Option<Arc<dyn CustomRouter>> {
        Some(self.0.clone())
    }
}

fn build_runtime(
    case: &JsValue,
    invoker: Arc<FakeInvoker>,
    events: Arc<Mutex<Vec<JsValue>>>,
    clock: Arc<dyn Fn() -> f64 + Send + Sync>,
) -> RouterRuntime {
    let config = RouterConfig::from_snapshot(field(case, "snapshot"));
    let denied: Vec<String> = array(field(case, "denied")).iter().map(text).collect();
    let custom = field(case, "custom").as_object().map(|custom| {
        Arc::new(FakeRegistry(Arc::new(FakeCustom {
            provider: field(&JsValue::Object(custom.clone()), "provider")
                .as_string()
                .cloned()
                .expect("custom provider"),
            model: field(&JsValue::Object(custom.clone()), "model")
                .as_string()
                .cloned()
                .expect("custom model"),
            error: field(case, "customError")
                .as_string()
                .map(|value| value.to_utf8().unwrap_or_default()),
        })))
    });
    let events_sink = events.clone();
    let custom_routers: Arc<dyn CustomRouterRegistry> = match custom {
        Some(registry) => registry,
        None => Arc::new(NoopCustomRouters),
    };
    RouterRuntime::with_deps(
        config,
        RouterDeps {
            model_invoker: invoker,
            custom_routers,
            events: Arc::new(move |event: JsValue| events_sink.lock().expect("events").push(event)),
            clock,
            // Hermetic: the oracle snapshots enable `stats`; the default
            // collector would create/read `$PILOT_HOME/router/stats.jsonl` (and
            // migrate a real legacy stats.json). Decision replay never observes
            // usage, so inject a disabled collector instead.
            stats: Some(Arc::new(Mutex::new(TokenStatsCollector::new(None, None)))),
            is_model_allowed: Arc::new(move |model: &RouterModelRef| {
                let key = format!(
                    "{}/{}",
                    model.provider.to_utf8().unwrap_or_default(),
                    model.model.to_utf8().unwrap_or_default()
                );
                !denied.contains(&key)
            }),
        },
    )
    .expect("router runtime")
}

fn error_value(error: &RouterRuntimeError) -> JsValue {
    let mut object = JsObject::new();
    object.insert("name".into(), "RouterRuntimeError".into());
    object.insert("code".into(), JsValue::String(error.code.into()));
    object.insert("message".into(), JsValue::String(error.message.clone()));
    JsValue::Object(object)
}

fn metadata(step: &JsValue) -> Option<JsObject> {
    field(step, "metadata").as_object().cloned()
}

async fn run_case(case: &JsValue, shared_runtime: bool) -> Result<(), String> {
    let invoker = Arc::new(FakeInvoker::from_case(case));
    let events = Arc::new(Mutex::new(Vec::new()));
    let now = Arc::new(AtomicU64::new(0));
    let clock_now = now.clone();
    let clock: Arc<dyn Fn() -> f64 + Send + Sync> =
        Arc::new(move || clock_now.load(Ordering::SeqCst) as f64);
    let mut runtime = Some(build_runtime(
        case,
        invoker.clone(),
        events.clone(),
        clock.clone(),
    ));
    let expected = array(field(case, "expected"));
    let mut last_decision = JsValue::Undefined;
    let mut event_index = 0usize;
    let mut actual_steps = Vec::new();

    for (index, step) in array(field(case, "steps")).iter().enumerate() {
        if !shared_runtime {
            runtime = Some(build_runtime(
                case,
                invoker.clone(),
                events.clone(),
                clock.clone(),
            ));
        }
        let runtime = runtime.as_ref().expect("runtime");
        let start = event_index;
        let kind = text(field(step, "kind"));
        let result = match kind.as_str() {
            "decide" => {
                let abort_mode = field(step, "abort").as_string().cloned();
                let abort = abort_mode.as_ref().map(|mode| {
                    let flag = AbortFlag::new();
                    if mode.equals_str("before") {
                        flag.abort();
                    }
                    flag
                });
                let input = RouterDecisionInput {
                    request: field(step, "request").clone(),
                    session_id: field(step, "sessionId")
                        .as_string()
                        .cloned()
                        .expect("sessionId"),
                    is_main_agent: field(step, "isMainAgent").as_bool().expect("isMainAgent"),
                    abort: abort.clone(),
                    metadata: metadata(step),
                };
                match runtime.decide(&input).await {
                    Ok(decision) => {
                        last_decision = decision.clone();
                        let mut object = JsObject::new();
                        object.insert("decision".into(), decision);
                        if let Some(flag) = &abort {
                            object.insert("callerAborted".into(), JsValue::Bool(flag.is_aborted()));
                        }
                        JsValue::Object(object)
                    }
                    Err(error) => {
                        let mut object = JsObject::new();
                        object.insert("error".into(), error_value(&error));
                        if let Some(flag) = &abort {
                            object.insert("callerAborted".into(), JsValue::Bool(flag.is_aborted()));
                        }
                        JsValue::Object(object)
                    }
                }
            }
            "advanceClock" => {
                let milliseconds = field(step, "ms").as_number().expect("ms") as u64;
                let value = now.fetch_add(milliseconds, Ordering::SeqCst) + milliseconds;
                let mut object = JsObject::new();
                object.insert("clock".into(), JsValue::Number(value as f64));
                JsValue::Object(object)
            }
            "invalidate" => {
                let session_id = field(step, "sessionId")
                    .as_string()
                    .cloned()
                    .expect("sessionId");
                let mut object = JsObject::new();
                object.insert("invalidated".into(), runtime.invalidate_sticky(&session_id));
                JsValue::Object(object)
            }
            "observeUsage" => {
                let session_id = field(step, "sessionId")
                    .as_string()
                    .cloned()
                    .expect("sessionId");
                runtime.observe_usage(&session_id, Some(field(step, "usage")));
                let mut object = JsObject::new();
                object.insert("observed".into(), JsValue::Bool(true));
                JsValue::Object(object)
            }
            "validate" => {
                let provider = field(step, "provider")
                    .as_string()
                    .cloned()
                    .expect("provider");
                let model = field(step, "model").as_string().cloned().expect("model");
                // Decode allow/deny arrays explicitly.
                let policy = field(step, "policy").as_object().map(|policy| {
                    let strings = |key: &str| {
                        array(policy.get(key).unwrap_or(&JsValue::Undefined))
                            .iter()
                            .filter_map(JsValue::as_string)
                            .cloned()
                            .collect()
                    };
                    pilotdeck_gateway::runtime::model::router::ManagedModelPolicy {
                        allow: strings("allow"),
                        deny: strings("deny"),
                    }
                });
                match runtime.validate_model_selection(&provider, &model, policy.as_ref()) {
                    Ok(()) => {
                        let mut object = JsObject::new();
                        object.insert("ok".into(), JsValue::Bool(true));
                        JsValue::Object(object)
                    }
                    Err(error) => {
                        let mut object = JsObject::new();
                        object.insert("error".into(), error_value(&error));
                        JsValue::Object(object)
                    }
                }
            }
            "materialize" => {
                let mut object = JsObject::new();
                object.insert(
                    "materialized".into(),
                    runtime.materialize_request(&last_decision, field(step, "request")),
                );
                JsValue::Object(object)
            }
            other => return Err(format!("unknown step {other}")),
        };

        let all_events = events.lock().expect("events").clone();
        event_index = all_events.len();
        let mut step_object = result.as_object().cloned().unwrap_or_default();
        step_object.insert(
            "judgeCalls".into(),
            JsValue::Number(invoker.calls.lock().expect("calls").len() as f64),
        );
        step_object.insert(
            "events".into(),
            JsValue::Array(all_events[start..event_index].to_vec()),
        );
        actual_steps.push(JsValue::Object(step_object));

        let expected_step = &expected[index];
        let actual_wire = wire(&actual_steps[index]);
        let expected_wire = wire(expected_step);
        if actual_wire != expected_wire {
            return Err(format!(
                "case {} step {index} ({kind}) mismatch\nexpected: {expected_wire}\nactual:   {actual_wire}",
                text(field(case, "id"))
            ));
        }
    }

    let prompts: Vec<JsValue> = invoker
        .calls
        .lock()
        .expect("calls")
        .iter()
        .map(|prompt| JsValue::String(prompt.as_str().into()))
        .collect();
    let expected_prompts = array(field(case, "judgeRequests")).to_vec();
    let actual = wire(&JsValue::Array(prompts));
    let expected = wire(&JsValue::Array(expected_prompts));
    if actual != expected {
        return Err(format!(
            "case {} judge prompts differ\nexpected: {expected}\nactual:   {actual}",
            text(field(case, "id"))
        ));
    }
    Ok(())
}

#[tokio::test]
async fn real_ts_router_decisions_and_policies_replay() {
    let oracle = oracle();
    let cases = array(field(&oracle, "cases"));
    assert_eq!(cases.len(), 28, "expected exactly 28 router cases");
    for case in cases {
        if let Err(error) = run_case(case, true).await {
            panic!("{error}");
        }
    }
}

#[tokio::test]
async fn expected_decision_mutation_is_detected() {
    let oracle = oracle();
    let original = array(field(&oracle, "cases"))[0].clone();
    let object = original.as_object().expect("case object");
    let mut steps = array(field(&original, "expected")).to_vec();
    let first = steps[0].as_object().cloned().expect("step");
    let decision = first
        .get("decision")
        .and_then(JsValue::as_object)
        .cloned()
        .expect("decision");
    let mut mutated = decision;
    mutated.insert("model".into(), "mutated".into());
    let mut step = first;
    step.insert("decision".into(), JsValue::Object(mutated));
    steps[0] = JsValue::Object(step);
    let mut case = object.clone();
    case.insert("expected".into(), JsValue::Array(steps));
    let case = JsValue::Object(case);
    assert!(
        run_case(&case, true).await.is_err(),
        "a mutated expected decision must fail replay"
    );
}

#[tokio::test]
async fn sticky_replay_requires_shared_runtime_state() {
    let oracle = oracle();
    let case = array(field(&oracle, "cases"))
        .iter()
        .find(|case| text(field(case, "id")) == "sticky")
        .expect("sticky case")
        .clone();
    assert!(
        run_case(&case, true).await.is_ok(),
        "shared runtime must match the TS sticky transcript"
    );
    assert!(
        run_case(&case, false).await.is_err(),
        "a fresh runtime per step must not reproduce sticky decisions"
    );
}
