//! Real TS RouterRuntime/health oracle. HTTP transport replay is separate so
//! its loopback permission requirement cannot silently skip the port corpus.
#[path = "common/baseline.rs"]
mod baseline;
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
fn appended(value: &JsValue, tag: &str) -> JsValue {
    let mut values = items(value).to_vec();
    values.push(obj(&[("mutated", tag.into())]));
    JsValue::Array(values)
}
fn tampered_usage(value: &JsValue) -> JsValue {
    let mut object = value.as_object().cloned().unwrap_or_default();
    object.insert("mutated".into(), JsValue::Bool(true));
    JsValue::Object(object)
}
fn compare_bytes(actual: &JsValue, expected: &JsValue, label: &str) -> Result<(), String> {
    let actual = stringify_json(actual).unwrap();
    let expected = stringify_json(expected).unwrap();
    if actual == expected {
        Ok(())
    } else {
        Err(format!("{label}: expected {expected:?}, got {actual:?}"))
    }
}

#[test]
fn health_steps_and_generation_port_match_real_ts() {
    let oracle = baseline::oracle(ORACLE);
    let cases = items(get(&oracle, "healthCases"));
    assert_eq!(cases.len(), 4);
    for case in cases {
        replay_health_case(case).unwrap();
    }
    let actions = items(get(&oracle, "portActions"));
    assert_eq!(actions.len(), 17);
    replay_port_actions(actions).unwrap();
}

fn replay_port_actions(actions: &[JsValue]) -> Result<(), String> {
    let now = Arc::new(AtomicU64::new(0));
    let time = now.clone();
    let port =
        NativeRouterProviderHealth::new(Arc::new(move || time.load(Ordering::SeqCst) as f64));
    for (index, action) in actions.iter().enumerate() {
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
            "skip" => compare_bytes(
                &JsValue::Bool(port.should_skip(&health)),
                get(action, "skip"),
                &format!("port action {index}"),
            )?,
            "time" => now.store(
                get(action, "now").as_number().unwrap() as u64,
                Ordering::SeqCst,
            ),
            "dispose" => port.dispose(),
            other => return Err(format!("port action {index}: unexpected {other}")),
        }
    }
    Ok(())
}

#[test]
fn expected_port_action_mutation_is_detected() {
    let oracle = baseline::oracle(ORACLE);
    let original: Vec<JsValue> = items(get(&oracle, "portActions")).to_vec();
    replay_port_actions(&original).unwrap();
    let mut mutated = original.clone();
    mutated[5] = merge(
        &mutated[5],
        &obj(&[(
            "skip",
            JsValue::Bool(!get(&mutated[5], "skip").as_bool().unwrap()),
        )]),
    );
    assert!(
        replay_port_actions(&mutated)
            .unwrap_err()
            .contains("port action 5")
    );
}
fn replay_health_case(case: &JsValue) -> Result<(), String> {
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
            other => return Err(format!("health action {index}: unexpected {other}")),
        };
        compare_bytes(
            &obj(&[("result", result), ("snapshot", tracker.snapshot())]),
            &expected[index],
            &format!("{} step {index}", text(get(case, "name"))),
        )?;
    }
    Ok(())
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
        replay_eligibility_case(case).unwrap();
    }
}

fn replay_eligibility_case(case: &JsValue) -> Result<(), String> {
    compare_bytes(
        &JsValue::Bool(is_fallback_eligible(get(case, "error"))),
        get(case, "eligible"),
        "fallback eligibility",
    )
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
                let event = if get(event, "type")
                    .as_string()
                    .is_some_and(|v| v.equals_str("usage"))
                {
                    let mut usage = get(event, "usage").as_object().cloned();
                    for field in items(get(&self.spec, "nanUsageFields")) {
                        usage.as_mut().expect("NaN usage object").insert(
                            field.as_string().unwrap().clone(),
                            JsValue::Number(f64::NAN),
                        );
                    }
                    usage
                        .map(|usage| merge(event, &obj(&[("usage", JsValue::Object(usage))])))
                        .unwrap_or_else(|| event.clone())
                } else {
                    event.clone()
                };
                sink(StreamOutput::Event(event));
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
    let oracle = baseline::oracle(ORACLE);
    let cases = items(get(&oracle, "executeCases"));
    assert_eq!(cases.len(), 46);
    for case in cases {
        replay_execute_case(case).await.unwrap();
    }
}

async fn replay_execute_case(case: &JsValue) -> Result<(), String> {
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
        project_path: None,
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
    compare_bytes(
        get(&actual, "calls"),
        get(case, "calls"),
        &format!("{name} calls"),
    )?;
    compare_bytes(
        get(&actual, "trace"),
        get(case, "trace"),
        &format!("{name} trace"),
    )?;
    let expected_usage = get(case, "sessionUsage");
    let mut actual_usage = get(&actual, "sessionUsage").clone();
    // JSON serializes NaN to null. Verify the actual non-JSON value first,
    // then normalize only explicitly registered fields for strict comparison.
    if let Some(mut usage) = actual_usage.as_object().cloned() {
        for key in items(get(spec, "nanUsageFields")) {
            let key = key.as_string().unwrap();
            if expected_usage.as_object().and_then(|v| v.get_key(key)) == Some(&JsValue::Null) {
                if !usage
                    .get_key(key)
                    .and_then(JsValue::as_number)
                    .is_some_and(f64::is_nan)
                {
                    return Err(format!("{name} usage: expected NaN for {key:?}"));
                }
                usage.insert(key.clone(), JsValue::Null);
            }
        }
        actual_usage = JsValue::Object(usage);
    }
    if &actual_usage != expected_usage {
        return Err(format!(
            "{name} usage: expected {expected_usage:?}, got {actual_usage:?}"
        ));
    }
    assert_eq!(request, original);
    Ok(())
}

#[tokio::test]
async fn expected_execute_mutations_are_detected() {
    let oracle = baseline::oracle(ORACLE);
    let original = &items(get(&oracle, "executeCases"))[0];
    replay_execute_case(original).await.unwrap();
    // Every comparison point of the execute family is pinned on its own: a
    // regression that makes just one of them compare a value with itself must
    // fail the control named by `point`.
    for (point, label) in [
        ("calls", "calls"),
        ("trace", "trace"),
        ("sessionUsage", "usage"),
    ] {
        let tampered = match point {
            "sessionUsage" => tampered_usage(get(original, point)),
            _ => appended(get(original, point), point),
        };
        let mutated = merge(original, &obj(&[(point, tampered)]));
        let error = replay_execute_case(&mutated)
            .await
            .expect_err(&format!("{point}: tampered expectation must be rejected"));
        assert!(error.contains(label), "{point}: {error}");
    }
}

#[test]
fn expected_eligibility_mutation_is_detected() {
    let oracle = baseline::oracle(ORACLE);
    let original = &items(get(&oracle, "eligibility"))[0];
    replay_eligibility_case(original).unwrap();
    let mutated = merge(
        original,
        &obj(&[(
            "eligible",
            JsValue::Bool(!get(original, "eligible").as_bool().unwrap()),
        )]),
    );
    assert!(
        replay_eligibility_case(&mutated)
            .unwrap_err()
            .contains("eligibility")
    );
}

#[test]
fn expected_health_state_mutation_is_detected() {
    let oracle = baseline::oracle(ORACLE);
    let original = &items(get(&oracle, "healthCases"))[0];
    replay_health_case(original).unwrap();
    let mut steps = items(get(original, "steps")).to_vec();
    assert_eq!(get(&steps[0], "result"), &JsValue::from("healthy"));
    steps[0] = merge(&steps[0], &obj(&[("result", "open".into())]));
    let mutated = merge(original, &obj(&[("steps", JsValue::Array(steps))]));
    assert!(replay_health_case(&mutated).unwrap_err().contains("step 0"));
}

#[tokio::test]
async fn expected_validation_code_mutation_is_detected() {
    let oracle = baseline::oracle(ORACLE);
    let original = &items(get(&oracle, "validationCases"))[0];
    http_tests::replay_validation_case(original).await.unwrap();
    let error = merge(get(original, "error"), &obj(&[("code", "mutated".into())]));
    let mutated = merge(original, &obj(&[("error", error)]));
    assert!(
        http_tests::replay_validation_case(&mutated)
            .await
            .unwrap_err()
            .contains("code")
    );
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

/// Re-record the real TS RouterRuntime/health corpus and compare its bytes.
#[test]
#[ignore = "requires PDRS_BASELINE (PilotDeck cc7c4e6 with node_modules) and PDRS_GIT"]
fn router_execute_fixtures_regenerate_identically_from_ts() {
    baseline::regenerates_identically("router-execute");
}
