//! Real TS RouterRuntime/health oracle. HTTP transport replay is separate so
//! its loopback permission requirement cannot silently skip the port corpus.
#[path = "common/baseline.rs"]
mod baseline;
#[path = "common/scratch.rs"]
mod scratch;
use pilotdeck_gateway::{
    host::ModuleError,
    runtime::model::{
        fallback::{RouterStreamFuture, is_fallback_eligible},
        health::{
            HealthOptions, NativeRouterProviderHealth, ProviderHealthInput, ProviderHealthTracker,
            RouterProviderHealth,
        },
        http::{ModelStreamError, StreamOptions, StreamOutput},
        router::{
            AbortFlag, ManagedModelPolicy, RouterConfig, RouterDeps, RouterExecuteContext,
            RouterExecuteError, RouterModelInvocation, RouterModelRef, RouterRuntime,
        },
    },
};
use pilotdeck_protocol::{JsObject, JsString, JsValue, stringify_json};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicU64, Ordering},
};
const ORACLE: &str = include_str!("fixtures/router-execute/oracle.json");
static UNDEFINED: JsValue = JsValue::Undefined;
fn get<'a>(value: &'a JsValue, key: &str) -> &'a JsValue {
    value.get(key).unwrap_or(&UNDEFINED)
}
fn items(value: &JsValue) -> &[JsValue] {
    match value {
        JsValue::Array(items) => items,
        _ => &[],
    }
}
fn text(value: &JsValue) -> String {
    value.as_string().unwrap().to_utf8().unwrap()
}
fn obj(fields: &[(&str, JsValue)]) -> JsValue {
    let mut object = JsObject::new();
    for (key, value) in fields {
        object.insert((*key).into(), value.clone());
    }
    JsValue::Object(object)
}
fn json(value: &JsValue) -> String {
    stringify_json(value).unwrap().unwrap()
}
fn merge(base: &JsValue, patch: &JsValue) -> JsValue {
    let mut base = base.as_object().cloned().unwrap_or_default();
    if let Some(patch) = patch.as_object() {
        for (k, v) in patch.entries() {
            base.insert(k.clone(), v.clone());
        }
    }
    JsValue::Object(base)
}
fn parsed(text: &str) -> JsValue {
    pilotdeck_protocol::parse_json(text).unwrap()
}
fn expected_bytes(actual: &JsValue, expected: &JsValue, label: &str) {
    assert_eq!(json(actual), json(expected), "{label}");
}

#[test]
fn health_steps_and_generation_port_match_real_ts() {
    let oracle = baseline::oracle(ORACLE);
    let cases = items(get(&oracle, "healthCases"));
    assert_eq!(cases.len(), 4);
    for case in cases {
        let now = Arc::new(AtomicU64::new(0));
        let time = now.clone();
        let options = get(case, "options");
        let defaults = HealthOptions::default();
        let mut tracker = ProviderHealthTracker::new(
            HealthOptions {
                degrade_threshold: get(options, "degradeThreshold")
                    .as_number()
                    .unwrap_or(defaults.degrade_threshold),
                open_threshold: get(options, "openThreshold")
                    .as_number()
                    .unwrap_or(defaults.open_threshold),
                open_duration_ms: get(options, "openDurationMs")
                    .as_number()
                    .unwrap_or(defaults.open_duration_ms),
                window_size: get(options, "windowSize")
                    .as_number()
                    .unwrap_or(defaults.window_size),
            },
            Arc::new(move || time.load(Ordering::SeqCst) as f64),
        );
        let actions = items(get(case, "actions"));
        let expected = items(get(case, "steps"));
        assert_eq!(actions.len(), expected.len());
        for (index, action) in actions.iter().enumerate() {
            let parts = items(action);
            let arg = parts.get(1).unwrap_or(&UNDEFINED);
            let provider = arg.as_string().cloned().unwrap_or_default();
            let result = match text(&parts[0]).as_str() {
                "time" => {
                    now.store(arg.as_number().unwrap() as u64, Ordering::SeqCst);
                    JsValue::Undefined
                }
                "state" => tracker.get_state(&provider).as_str().into(),
                "skip" => JsValue::Bool(tracker.should_skip(&provider)),
                "available" => JsValue::Bool(tracker.is_available(&provider)),
                "success" => {
                    tracker.record_success(&provider);
                    JsValue::Undefined
                }
                "failure" => {
                    tracker.record_failure(&provider);
                    JsValue::Undefined
                }
                "reset" => {
                    tracker.reset(&provider);
                    JsValue::Undefined
                }
                "resetAll" => {
                    tracker.reset_all();
                    JsValue::Undefined
                }
                other => panic!("{other}"),
            };
            expected_bytes(
                &obj(&[("result", result), ("snapshot", tracker.snapshot())]),
                &expected[index],
                &format!("{} step {index}", text(get(case, "name"))),
            );
        }
    }
    let now = Arc::new(AtomicU64::new(0));
    let time = now.clone();
    let port =
        NativeRouterProviderHealth::new(Arc::new(move || time.load(Ordering::SeqCst) as f64));
    let actions = items(get(&oracle, "portActions"));
    assert_eq!(actions.len(), 17);
    for action in actions {
        let input = get(action, "input");
        let health = ProviderHealthInput {
            session_id: get(input, "sessionId")
                .as_string()
                .cloned()
                .unwrap_or_default(),
            provider_id: get(input, "providerId")
                .as_string()
                .cloned()
                .unwrap_or_default(),
            provider_generation: get(input, "providerGeneration").clone(),
        };
        match text(get(action, "action")).as_str() {
            "failure" => port.record_failure(&health),
            "success" => port.record_success(&health),
            "skip" => assert_eq!(
                port.should_skip(&health),
                get(action, "skip").as_bool().unwrap()
            ),
            "time" => now.store(
                get(action, "now").as_number().unwrap() as u64,
                Ordering::SeqCst,
            ),
            "dispose" => port.dispose(),
            other => panic!("{other}"),
        }
    }
}
#[tokio::test(start_paused = true)]
async fn health_cooldown_uses_paused_clock() {
    let start = tokio::time::Instant::now();
    let mut tracker = ProviderHealthTracker::new(
        HealthOptions::default(),
        Arc::new(move || start.elapsed().as_millis() as f64),
    );
    let key = "p".into();
    for _ in 0..5 {
        tracker.record_failure(&key);
    }
    tokio::time::advance(std::time::Duration::from_millis(29999)).await;
    assert!(tracker.should_skip(&key));
    tokio::time::advance(std::time::Duration::from_millis(1)).await;
    assert!(!tracker.should_skip(&key));
    tracker.record_failure(&key);
    assert!(tracker.should_skip(&key));
}
#[test]
fn fallback_admission_matches_real_ts() {
    let oracle = baseline::oracle(ORACLE);
    let cases = items(get(&oracle, "eligibility"));
    assert_eq!(cases.len(), 60);
    for case in cases {
        assert_eq!(
            is_fallback_eligible(get(case, "error")),
            get(case, "eligible").as_bool().unwrap(),
            "{}",
            json(case)
        );
    }
}

type Trace = Arc<Mutex<Vec<JsValue>>>;
struct Invoker {
    spec: JsValue,
    calls: Trace,
    generation: Mutex<JsValue>,
}
impl RouterModelInvocation for Invoker {
    fn get_capabilities(&self, _: &JsString, _: &JsString) -> Result<JsValue, ModuleError> {
        Ok(parsed(r#"{"maxOutputTokens":64}"#))
    }
    fn get_multimodal(&self, provider: &JsString, _: &JsString) -> Result<JsValue, ModuleError> {
        let input = get(&self.spec, "media")
            .as_object()
            .and_then(|media| media.get_key(provider))
            .cloned()
            .unwrap_or_else(|| parsed(r#"["text","image","pdf","audio"]"#));
        Ok(obj(&[("input", input)]))
    }
    fn get_provider_protocol(&self, _: &JsString) -> JsString {
        "anthropic".into()
    }
    fn get_provider_generation(&self, _: &JsString) -> JsValue {
        self.generation.lock().unwrap().clone()
    }
    fn stream<'a>(
        &'a self,
        request: &'a JsValue,
        _: &'a StreamOptions,
        sink: &'a mut (dyn FnMut(StreamOutput) + Send),
    ) -> RouterStreamFuture<'a> {
        Box::pin(async move {
            let index = {
                let mut calls = self.calls.lock().unwrap();
                let index = calls.len();
                calls.push(request.clone());
                index
            };
            let script = items(get(&self.spec, "scripts"))
                .get(index)
                .expect("unexpected invocation");
            if !get(&self.spec, "changeGeneration").is_undefined() {
                *self.generation.lock().unwrap() = get(&self.spec, "changeGeneration").clone();
            }
            for event in items(get(script, "events")) {
                sink(StreamOutput::Event(event.clone()));
            }
            let thrown = get(script, "throw");
            if !thrown.is_undefined() {
                if get(thrown, "name")
                    .as_string()
                    .is_some_and(|name| name.equals_str("ModelRequestError"))
                {
                    return Err(ModelStreamError::Request {
                        code: get(thrown, "code").as_string().unwrap().clone(),
                        message: get(thrown, "message").as_string().unwrap().clone(),
                        details: (!get(thrown, "details").is_undefined())
                            .then(|| Box::new(get(thrown, "details").clone())),
                    });
                }
                return Err(ModelStreamError::Thrown {
                    name: get(thrown, "name").as_string().unwrap().clone(),
                    message: get(thrown, "message").as_string().unwrap().clone(),
                });
            }
            Ok(())
        })
    }
}
struct TracedHealth {
    inner: NativeRouterProviderHealth,
    trace: Trace,
}
impl TracedHealth {
    fn emit(&self, action: &str, input: &ProviderHealthInput, result: JsValue) {
        self.trace.lock().unwrap().push(obj(&[
            ("kind", "health".into()),
            ("action", action.into()),
            (
                "input",
                obj(&[
                    ("sessionId", JsValue::String(input.session_id.clone())),
                    ("providerId", JsValue::String(input.provider_id.clone())),
                    ("providerGeneration", input.provider_generation.clone()),
                ]),
            ),
            ("result", result),
        ]));
    }
}
impl RouterProviderHealth for TracedHealth {
    fn should_skip(&self, input: &ProviderHealthInput) -> bool {
        let result = self.inner.should_skip(input);
        self.emit("shouldSkip", input, JsValue::Bool(result));
        result
    }
    fn record_failure(&self, input: &ProviderHealthInput) {
        self.inner.record_failure(input);
        self.emit("recordFailure", input, JsValue::Undefined);
    }
    fn record_success(&self, input: &ProviderHealthInput) {
        self.inner.record_success(input);
        self.emit("recordSuccess", input, JsValue::Undefined);
    }
}
#[tokio::test]
async fn execute_port_sequences_match_real_ts() {
    let scratch = scratch::Scratch::new("router-execute");
    let oracle = baseline::oracle(ORACLE);
    let cases = items(get(&oracle, "executeCases"));
    assert_eq!(cases.len(), 32);
    for case in cases {
        let spec = get(case, "spec");
        let name = text(get(spec, "name"));
        let trace: Trace = Arc::default();
        let calls: Trace = Arc::default();
        let invoker = Arc::new(Invoker {
            spec: spec.clone(),
            calls: calls.clone(),
            generation: Mutex::new(get(spec, "generation").clone()),
        });
        let config = merge(
            &parsed(
                r#"{"enabled":true,"fallback":{"default":[{"provider":"b","model":"m"}]},"transientRetry":{"enabled":false},"zeroUsageRetry":{"enabled":false}}"#,
            ),
            get(spec, "config"),
        );
        let config = RouterConfig::from_snapshot(&obj(&[("config", obj(&[("router", config)]))]));
        let mut deps = RouterDeps::new(invoker);
        let events = trace.clone();
        deps.events = Arc::new(move |value| {
            events
                .lock()
                .unwrap()
                .push(obj(&[("kind", "router".into()), ("value", value)]))
        });
        deps.clock = Arc::new(|| 0.0);
        if get(spec, "policyFlips").as_bool() == Some(true) {
            let reads = AtomicU64::new(0);
            deps.is_model_allowed = Arc::new(move |_| reads.fetch_add(1, Ordering::SeqCst) == 0);
        }
        let health = TracedHealth {
            inner: NativeRouterProviderHealth::new(Arc::new(|| 0.0)),
            trace: trace.clone(),
        };
        for provider in items(get(spec, "seedOpen")) {
            let input = ProviderHealthInput {
                session_id: "s".into(),
                provider_id: provider.as_string().unwrap().clone(),
                provider_generation: JsValue::Undefined,
            };
            for _ in 0..5 {
                health.inner.record_failure(&input);
            }
        }
        let router = RouterRuntime::with_deps(config, deps)
            .unwrap()
            .with_provider_health(Arc::new(health));
        let decision = merge(
            &parsed(
                r#"{"provider":"a","model":"m","scenarioType":"default","isSubagent":false,"orchestrating":false,"resolvedFrom":"scenario","mutations":{}}"#,
            ),
            get(spec, "decision"),
        );
        let request = merge(
            &parsed(
                r#"{"provider":"a","model":"m","messages":[{"role":"user","content":[{"type":"text","text":"hello"}]}],"maxOutputTokens":100}"#,
            ),
            get(spec, "request"),
        );
        let original = request.clone();
        let abort = AbortFlag::new();
        if get(spec, "abortBefore").as_bool() == Some(true) {
            abort.abort();
        }
        let policy = get(spec, "managedModelPolicy");
        let policy = policy.as_object().map(|_| ManagedModelPolicy {
            allow: items(get(policy, "allow"))
                .iter()
                .map(|v| v.as_string().unwrap().clone())
                .collect(),
            deny: items(get(policy, "deny"))
                .iter()
                .map(|v| v.as_string().unwrap().clone())
                .collect(),
        });
        let ctx = RouterExecuteContext {
            session_id: "s".into(),
            turn_id: "t".into(),
            fallback_models: items(get(spec, "fallbackModels"))
                .iter()
                .map(|v| RouterModelRef::from_value(v).unwrap())
                .collect(),
            managed_model_policy: policy,
            stream_options: StreamOptions {
                abort: Some(abort.clone()),
                random: Arc::new(|| 0.5),
                ..Default::default()
            },
        };
        let result = router
            .execute(&decision, &request, &ctx, &mut |value| {
                let cancel = get(spec, "abortAfterText").as_bool() == Some(true)
                    && get(&value, "type")
                        .as_string()
                        .is_some_and(|v| v.equals_str("text_delta"));
                trace
                    .lock()
                    .unwrap()
                    .push(obj(&[("kind", "event".into()), ("value", value)]));
                if cancel {
                    abort.abort();
                }
            })
            .await;
        if let Err(error) = result {
            let value = match error {
                RouterExecuteError::Router(error) => obj(&[
                    ("name", "RouterRuntimeError".into()),
                    ("message", JsValue::String(error.message)),
                    ("code", error.code.into()),
                ]),
                RouterExecuteError::Model(error) => thrown(&error),
            };
            trace
                .lock()
                .unwrap()
                .push(obj(&[("kind", "thrown".into()), ("value", value)]));
        }
        let actual = obj(&[
            ("calls", JsValue::Array(calls.lock().unwrap().clone())),
            ("trace", JsValue::Array(trace.lock().unwrap().clone())),
            (
                "sessionUsage",
                router
                    .get_session_usage(&"s".into())
                    .unwrap_or(JsValue::Undefined),
            ),
        ]);
        std::fs::write(scratch.path().join(format!("{name}.json")), json(&actual)).unwrap();
        expected_bytes(
            get(&actual, "calls"),
            get(case, "calls"),
            &format!("{name} calls"),
        );
        expected_bytes(
            get(&actual, "trace"),
            get(case, "trace"),
            &format!("{name} trace"),
        );
        assert_eq!(
            get(&actual, "sessionUsage"),
            get(case, "sessionUsage"),
            "{name} usage"
        );
        assert_eq!(request, original);
    }
}

#[path = "common/router_execute_http.rs"]
mod http_tests;

// The AbortFlag path models AbortController.abort()'s DOMException, whose
// standard legacy code is observable in the TS recorder (20 / ABORT_ERR).
fn thrown(error: &ModelStreamError) -> JsValue {
    obj(&[
        ("name", JsValue::String(error.name())),
        ("message", JsValue::String(error.message())),
        (
            "code",
            if error.name().equals_str("AbortError") {
                JsValue::Number(20.0)
            } else {
                JsValue::Undefined
            },
        ),
    ])
}
