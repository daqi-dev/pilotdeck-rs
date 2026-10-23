//! Host model module port against the fixed TypeScript composition
//! (`tests/fixtures/host-model-port`). `record.mts` drives the real TS
//! `createRouterModelInvokerPort` → `createDurableModelInvokerPort` →
//! `bindTurn` chain exactly the way `sidecarDefaultModuleDispatcher.ts` drives
//! it; this replay drives `host::modules::model_port` the same way (prepare,
//! metadata lookups, materialize, iterator pulls, explicit close) and compares
//! the trace, the invoker calls, the HTTP request records and the transcript
//! bytes. HTTP cases point the providers at the scripted loopback mock from
//! `tests/common/model_mock.rs` through the real `HttpRouterInvocation`, like
//! the router-execute and model-stream fixtures; a cancelled stream must close
//! the provider request (`clientClosed`) and the recorder must not advance
//! past the last pulled event.
#[path = "common/baseline.rs"]
mod baseline;
#[path = "common/model_mock.rs"]
mod model_mock;
use pilotdeck_gateway::{
    host::{
        ModuleError,
        modules::{
            model::{PrepareInput, StreamInput, TurnContext},
            model_port::{self, DurableRecorder, RouterModelPortOptions},
        },
    },
    runtime::model::{
        fallback::{HttpRouterInvocation, RouterStreamFuture},
        http::{HyperModelTransport, ModelStreamError, StreamOptions, StreamOutput},
        router::{
            ManagedModelPolicy, RouterConfig, RouterDeps, RouterModelInvocation, RouterRuntime,
        },
    },
    storage::recorder::Recorder,
    storage::{JsonlTranscriptStore, TranscriptKey},
};
use pilotdeck_protocol::{JsObject, JsString, JsValue, parse_json, stringify_json};
use std::{
    fs,
    path::{Component, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
};

const ORACLE: &str = include_str!("fixtures/host-model-port/oracle.json");
const SESSION: &str = "s1";
const NOW: &str = "2026-10-18T00:00:00.000Z";
const PROJECT_ROOT: &str = "/pdrs-fixture/project";
static UNDEFINED: JsValue = JsValue::Undefined;
type Trace = Arc<Mutex<Vec<JsValue>>>;

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
        for (key, value) in patch.entries() {
            base.insert(key.clone(), value.clone());
        }
    }
    JsValue::Object(base)
}
fn parsed(text: &str) -> JsValue {
    parse_json(text).unwrap()
}
fn appended(value: &JsValue, tag: &str) -> JsValue {
    let mut values = items(value).to_vec();
    values.push(obj(&[("mutated", tag.into())]));
    JsValue::Array(values)
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
fn case_name(case: &JsValue) -> String {
    text(get(get(case, "spec"), "name"))
}

/// Per-case exclusive scratch, like `tests/recorder.rs`. Removed on drop.
struct Scratch(PathBuf);
impl Scratch {
    fn new(name: &str) -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../_tmp/20261023-a69-pr1-model-port/test-scratch");
        let mut clean = PathBuf::new();
        for component in root.components() {
            match component {
                Component::ParentDir => {
                    clean.pop();
                }
                Component::CurDir => {}
                other => clean.push(other.as_os_str()),
            }
        }
        let root = clean;
        fs::create_dir_all(&root).unwrap();
        loop {
            let dir = root.join(format!(
                "{name}-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            match fs::create_dir(&dir) {
                Ok(()) => return Self(dir),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => panic!("create exclusive scratch: {error}"),
            }
        }
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// The scripted `RouterModelInvocation` the TS generator installs: capabilities
/// and multimodal input come from the shared provider config, the stream
/// replays the spec's scripts (`events`, then an optional `throw`).
struct ScriptedInvoker {
    providers: JsValue,
    spec: JsValue,
    calls: Trace,
}
impl RouterModelInvocation for ScriptedInvoker {
    fn get_capabilities(
        &self,
        provider: &JsString,
        model: &JsString,
    ) -> Result<JsValue, ModuleError> {
        let capabilities = self
            .providers
            .as_object()
            .and_then(|providers| providers.get_key(provider))
            .and_then(|provider| provider.get("models"))
            .and_then(JsValue::as_object)
            .and_then(|models| models.get_key(model))
            .and_then(|model| model.get("capabilities"))
            .cloned();
        capabilities.ok_or_else(|| {
            ModuleError::new(format!(
                "Unknown model {}/{}",
                provider.to_utf8().unwrap_or_default(),
                model.to_utf8().unwrap_or_default()
            ))
        })
    }
    fn get_multimodal(&self, provider: &JsString, _: &JsString) -> Result<JsValue, ModuleError> {
        let input = get(&self.spec, "media")
            .as_object()
            .and_then(|media| media.get_key(provider))
            .cloned()
            .unwrap_or_else(|| parsed(r#"["text","image"]"#));
        Ok(obj(&[("input", input)]))
    }
    fn get_provider_protocol(&self, _: &JsString) -> JsString {
        "anthropic".into()
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
            for event in items(get(script, "events")) {
                sink(StreamOutput::Event(event.clone()));
            }
            let thrown = get(script, "throw");
            if !thrown.is_undefined() {
                return Err(ModelStreamError::Thrown {
                    name: get(thrown, "name").as_string().unwrap().clone(),
                    message: get(thrown, "message").as_string().unwrap().clone(),
                });
            }
            Ok(())
        })
    }
}

/// Counts invocations around the real HTTP invocation provider, like
/// `tests/common/router_execute_http.rs`.
struct RecordingHttp {
    inner: HttpRouterInvocation,
    calls: Trace,
}
impl RouterModelInvocation for RecordingHttp {
    fn get_capabilities(
        &self,
        provider: &JsString,
        model: &JsString,
    ) -> Result<JsValue, ModuleError> {
        self.inner.get_capabilities(provider, model)
    }
    fn get_multimodal(
        &self,
        provider: &JsString,
        model: &JsString,
    ) -> Result<JsValue, ModuleError> {
        self.inner.get_multimodal(provider, model)
    }
    fn get_provider_protocol(&self, provider: &JsString) -> JsString {
        self.inner.get_provider_protocol(provider)
    }
    fn stream<'a>(
        &'a self,
        request: &'a JsValue,
        options: &'a StreamOptions,
        sink: &'a mut (dyn FnMut(StreamOutput) + Send),
    ) -> RouterStreamFuture<'a> {
        self.calls.lock().unwrap().push(request.clone());
        self.inner.stream(request, options, sink)
    }
}

/// `errorOf`: the module wire face of an error is `{message, code?}`.
fn error_entry(error: &ModuleError) -> JsValue {
    let mut value = JsObject::new();
    value.insert("message".into(), JsValue::String(error.message.clone()));
    if let Some(code) = &error.code {
        value.insert("code".into(), JsValue::String(code.clone()));
    }
    obj(&[("kind", "error".into()), ("value", JsValue::Object(value))])
}

/// The request every case prepares (`record.mts` `baseRequest`).
fn base_request() -> JsValue {
    parsed(
        r#"{"provider":"a","model":"m","messages":[{"role":"user","content":[{"type":"text","text":"hello"}]}],"maxOutputTokens":100}"#,
    )
}

/// `{sessionId, turnId, runId, ...spec.context}`, like the TS driver.
fn turn_context(spec: &JsValue, turn_id: &JsString) -> JsObject {
    let mut context = JsObject::new();
    context.insert("sessionId".into(), SESSION.into());
    context.insert("turnId".into(), JsValue::String(turn_id.clone()));
    context.insert("runId".into(), JsValue::String(turn_id.clone()));
    if let Some(extra) = get(spec, "context").as_object() {
        for (key, value) in extra.entries() {
            context.insert(key.clone(), value.clone());
        }
    }
    context
}

async fn replay_case(case: &JsValue, oracle: &JsValue) -> Result<(), String> {
    let spec = get(case, "spec").clone();
    let name = text(get(&spec, "name"));
    let http = get(&spec, "http").as_bool().unwrap_or(false);
    let trace: Trace = Arc::default();
    let calls: Trace = Arc::default();
    let scratch = Scratch::new(&name);
    let transcript_path = scratch.0.join(format!("{name}-{SESSION}.jsonl"));

    let providers = get(oracle, "providers").clone();
    let (model_config, invoker, http_state) = if http {
        let exchange = Arc::new(Mutex::new(model_mock::Exchange {
            replies: items(get(&spec, "replies"))
                .iter()
                .map(|reply| {
                    merge(
                        reply,
                        &obj(&[(
                            "chunks",
                            JsValue::Array(
                                items(get(reply, "chunks"))
                                    .iter()
                                    .map(|chunk| obj(&[("text", chunk.clone())]))
                                    .collect(),
                            ),
                        )]),
                    )
                })
                .collect(),
            ..Default::default()
        }));
        let excluded = items(get(oracle, "transportHeaders"))
            .iter()
            .map(text)
            .collect();
        let server = model_mock::start_mock(exchange.clone(), excluded, None);
        let mut configured = providers.as_object().cloned().unwrap();
        for id in ["a", "b", "c"] {
            let mut provider = configured
                .get_key(&id.into())
                .unwrap()
                .as_object()
                .unwrap()
                .clone();
            provider.insert(
                "url".into(),
                format!("{}/{id}/v1", server.origin).as_str().into(),
            );
            configured.insert(id.into(), JsValue::Object(provider));
        }
        let model_config = obj(&[("providers", JsValue::Object(configured))]);
        let delegate = Arc::new(ScriptedInvoker {
            providers: providers.clone(),
            spec: spec.clone(),
            calls: Arc::default(),
        });
        let adapter = Arc::new(RecordingHttp {
            inner: HttpRouterInvocation::new(
                delegate,
                Arc::new(HyperModelTransport::new()),
                model_config.clone(),
            ),
            calls: calls.clone(),
        });
        (
            model_config,
            adapter as Arc<dyn RouterModelInvocation>,
            Some((server, exchange)),
        )
    } else {
        (
            obj(&[("providers", providers.clone())]),
            Arc::new(ScriptedInvoker {
                providers: providers.clone(),
                spec: spec.clone(),
                calls: calls.clone(),
            }) as Arc<dyn RouterModelInvocation>,
            None,
        )
    };

    let mut deps = RouterDeps::new(invoker);
    let events = trace.clone();
    deps.events = Arc::new(move |value| {
        events
            .lock()
            .unwrap()
            .push(obj(&[("kind", "router".into()), ("value", value)]));
    });
    deps.clock = Arc::new(|| 0.0);
    // `record.mts` `createRouterRuntime` configuration.
    let base = obj(&[
        ("enabled", JsValue::Bool(true)),
        (
            "scenarios",
            obj(&[(
                "default",
                parsed(r#"{"id":"a/m","provider":"a","model":"m"}"#),
            )]),
        ),
        (
            "fallback",
            obj(&[(
                "default",
                JsValue::Array(vec![parsed(r#"{"id":"b/m","provider":"b","model":"m"}"#)]),
            )]),
        ),
        ("transientRetry", obj(&[("enabled", JsValue::Bool(false))])),
        ("zeroUsageRetry", obj(&[("enabled", JsValue::Bool(false))])),
    ]);
    let snapshot = obj(&[(
        "config",
        obj(&[("router", merge(&base, get(&spec, "config")))]),
    )]);
    let router = Arc::new(
        RouterRuntime::with_deps(RouterConfig::from_snapshot(&snapshot), deps)
            .expect("router runtime"),
    );

    let ids = AtomicUsize::new(0);
    let recorder = Arc::new(Recorder::with_sources(
        Arc::new(JsonlTranscriptStore::new()),
        TranscriptKey {
            project_root: PathBuf::from(PROJECT_ROOT),
            pilot_home: scratch.0.join("home"),
            session_id: SESSION.into(),
            transcript_path: transcript_path.clone(),
        },
        Arc::new(|| Ok(NOW.to_string())),
        Arc::new(move || Ok(format!("id-{}", ids.fetch_add(1, Ordering::SeqCst) + 1))),
    ));

    let options = RouterModelPortOptions {
        is_main_agent: get(&spec, "isMainAgent").as_bool().unwrap_or(true),
        project_path: None,
        fallback_models: Vec::new(),
        managed_model_policy: get(&spec, "managedModelPolicy").as_object().map(|policy| {
            let policy = &JsValue::Object(policy.clone());
            ManagedModelPolicy {
                allow: items(get(policy, "allow"))
                    .iter()
                    .filter_map(|value| value.as_string().cloned())
                    .collect(),
                deny: items(get(policy, "deny"))
                    .iter()
                    .filter_map(|value| value.as_string().cloned())
                    .collect(),
            }
        }),
        agent_model: Some(("a".into(), "m".into())),
        agent_max_context_tokens: (!get(&spec, "agentMaxContextTokens").is_undefined())
            .then(|| get(&spec, "agentMaxContextTokens").clone()),
    };

    let session: JsString = SESSION.into();
    let turns = get(&spec, "turns").as_number().unwrap_or(1.0) as usize;
    for turn in 1..=turns {
        let turn_id: JsString = format!("t{turn}").into();
        recorder.start_turn(&session, &turn_id).expect("start turn");
        let model_port::ComposedPorts {
            execution,
            metadata,
            materializer,
        } = model_port::ports(
            router.clone(),
            model_config.clone(),
            Some(DurableRecorder {
                recorder: recorder.clone(),
                recording_session_id: None,
            }),
            options.clone(),
            &session,
        );
        if get(&spec, "invalidateProbe") == &JsValue::Bool(true) && turn == 2 {
            trace.lock().unwrap().push(obj(&[
                ("kind", "invalidate".into()),
                ("value", router.invalidate_sticky(&session)),
            ]));
        }
        let repeats = get(&spec, "repeat").as_number().unwrap_or(1.0) as usize;
        for _ in 0..repeats {
            let context = turn_context(&spec, &turn_id);
            let prepared = match execution
                .prepare(PrepareInput {
                    request: base_request(),
                    context: context.clone(),
                })
                .await
            {
                Ok(prepared) => prepared,
                Err(error) => {
                    trace.lock().unwrap().push(error_entry(&error));
                    continue;
                }
            };
            trace.lock().unwrap().push(obj(&[
                ("kind", "prepared".into()),
                (
                    "value",
                    obj(&[
                        ("request", prepared.request.clone()),
                        ("provider", JsValue::String(prepared.provider.clone())),
                        ("model", JsValue::String(prepared.model.clone())),
                        (
                            "opaque",
                            prepared.opaque.clone().unwrap_or(JsValue::Undefined),
                        ),
                    ]),
                ),
            ]));
            if get(&spec, "metadata") == &JsValue::Bool(true) {
                let metadata = metadata.as_ref().expect("composed metadata port");
                for (provider, model) in [("a", "m"), ("b", "m"), ("x", "m")] {
                    let provider: JsString = provider.into();
                    let model: JsString = model.into();
                    let lookups = [
                        (
                            "maxContextTokens",
                            metadata
                                .get_model_max_context_tokens(&provider, &model)
                                .expect("metadata lookup")
                                .unwrap_or(JsValue::Null),
                        ),
                        (
                            "maxOutputTokens",
                            metadata
                                .get_model_max_output_tokens(&provider, &model)
                                .expect("metadata lookup")
                                .unwrap_or(JsValue::Null),
                        ),
                        (
                            "tokenLimits",
                            metadata
                                .get_model_token_limits(&provider, &model)
                                .expect("metadata lookup")
                                .unwrap_or(JsValue::Null),
                        ),
                        (
                            "protocol",
                            metadata
                                .get_model_protocol(&provider)
                                .expect("metadata lookup")
                                .unwrap_or(JsValue::Null),
                        ),
                        (
                            "supportsPromptCache",
                            metadata
                                .get_model_supports_prompt_cache(&provider, &model)
                                .expect("metadata lookup")
                                .unwrap_or(JsValue::Null),
                        ),
                    ];
                    for (field, value) in lookups {
                        trace.lock().unwrap().push(obj(&[
                            ("kind", "metadata".into()),
                            ("field", field.into()),
                            ("provider", JsValue::String(provider.clone())),
                            ("model", JsValue::String(model.clone())),
                            ("value", value),
                        ]));
                    }
                }
            }
            if get(&spec, "materialize") == &JsValue::Bool(true) {
                let candidate = merge(
                    &base_request(),
                    &obj(&[(
                        "messages",
                        parsed(
                            r#"[{"role":"user","content":[{"type":"text","text":"candidate"}]}]"#,
                        ),
                    )]),
                );
                let materialized = materializer
                    .as_ref()
                    .expect("composed materializer")
                    .materialize(prepared.clone(), candidate.as_object().unwrap().clone())
                    .await
                    .expect("materialize");
                trace.lock().unwrap().push(obj(&[
                    ("kind", "materialized".into()),
                    ("value", materialized),
                ]));
            }
            let mut stream = match execution
                .stream(StreamInput {
                    prepared: prepared.clone(),
                    context: context.clone(),
                })
                .await
            {
                Ok(stream) => stream,
                Err(error) => {
                    trace.lock().unwrap().push(error_entry(&error));
                    continue;
                }
            };
            let pulls = get(&spec, "pulls").as_number();
            let mut pulled = 0.0f64;
            let mut drained = false;
            while pulls.is_none_or(|limit| pulled < limit) {
                match stream.next().await {
                    Err(error) => {
                        trace.lock().unwrap().push(error_entry(&error));
                        break;
                    }
                    Ok(None) => {
                        trace.lock().unwrap().push(obj(&[("kind", "done".into())]));
                        drained = true;
                        break;
                    }
                    Ok(Some(event)) => {
                        trace
                            .lock()
                            .unwrap()
                            .push(obj(&[("kind", "event".into()), ("value", event)]));
                        pulled += 1.0;
                    }
                }
            }
            if pulls.is_some() && !drained {
                match stream.close().await {
                    Ok(()) => {
                        trace
                            .lock()
                            .unwrap()
                            .push(obj(&[("kind", "closed".into())]));
                    }
                    Err(error) => {
                        trace.lock().unwrap().push(error_entry(&error));
                    }
                }
            }
        }
    }

    if let Some((_server, exchange)) = &http_state {
        let holds = items(get(&spec, "replies")).iter().any(|reply| {
            get(reply, "end")
                .as_string()
                .is_some_and(|end| end.equals_str("hold"))
        });
        if holds {
            tokio::time::timeout(std::time::Duration::from_secs(5), async {
                loop {
                    let closed =
                        exchange.lock().unwrap().requests.iter().any(|request| {
                            request.get("clientClosed") == Some(&JsValue::Bool(true))
                        });
                    if closed {
                        break;
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
                }
            })
            .await
            .map_err(|_| {
                format!("{name}: cancelled stream must close the held provider request")
            })?;
        }
        let expected_count = items(get(&spec, "replies")).len();
        let requests = exchange.lock().unwrap().requests.clone();
        if requests.len() != expected_count {
            return Err(format!(
                "{name} HTTP requests: expected {expected_count}, got {}",
                requests.len()
            ));
        }
        compare_bytes(
            &JsValue::Array(requests.into_iter().map(JsValue::Object).collect()),
            get(case, "requests"),
            &format!("{name} HTTP requests"),
        )?;
    }
    let origin = http_state.as_ref().map(|(server, _)| server.origin.clone());
    let normalize = |value: String| match &origin {
        Some(origin) => value.replace(origin, "$MOCK"),
        None => value,
    };
    let trace_value = JsValue::Array(trace.lock().unwrap().clone());
    compare_bytes(
        &JsValue::String(normalize(json(&trace_value)).into()),
        &JsValue::String(json(get(case, "trace")).into()),
        &format!("{name} trace"),
    )?;
    compare_bytes(
        &JsValue::Array(calls.lock().unwrap().clone()),
        get(case, "calls"),
        &format!("{name} invocations"),
    )?;
    let transcript = normalize(fs::read_to_string(&transcript_path).unwrap_or_default());
    let expected_transcript = text(get(case, "transcript"));
    if transcript != expected_transcript {
        return Err(format!(
            "{name} transcript: expected {expected_transcript:?}, got {transcript:?}"
        ));
    }
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn port_sequences_match_real_ts() {
    let oracle = baseline::oracle(ORACLE);
    let cases = items(get(&oracle, "cases"));
    assert_eq!(cases.len(), 17);
    for case in cases {
        replay_case(case, &oracle).await.unwrap();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn expected_oracle_mutations_are_detected() {
    let oracle = baseline::oracle(ORACLE);
    let cases = items(get(&oracle, "cases"));
    // Pin every scripted comparison point under its own label; a regression
    // that self-compares just one of them must fail under that name.
    let scripted = cases
        .iter()
        .find(|case| case_name(case) == "text-stream")
        .unwrap()
        .clone();
    replay_case(&scripted, &oracle).await.unwrap();
    for (point, label) in [
        ("trace", "trace"),
        ("calls", "invocations"),
        ("transcript", "transcript"),
    ] {
        let tampered = match point {
            "transcript" => JsValue::String(format!("{}\n", text(get(&scripted, point))).into()),
            _ => appended(get(&scripted, point), point),
        };
        let mutated = merge(&scripted, &obj(&[(point, tampered)]));
        let error = replay_case(&mutated, &oracle)
            .await
            .expect_err("tampered expectation must be rejected");
        assert!(
            error.contains(&format!("text-stream {label}")),
            "{point}: {error}"
        );
    }
    // The HTTP request record is pinned separately, like router-execute.
    let http_case = cases
        .iter()
        .find(|case| case_name(case) == "http-text-stream")
        .unwrap()
        .clone();
    replay_case(&http_case, &oracle).await.unwrap();
    let mutated = merge(
        &http_case,
        &obj(&[(
            "requests",
            appended(get(&http_case, "requests"), "requests"),
        )]),
    );
    let error = replay_case(&mutated, &oracle)
        .await
        .expect_err("tampered expectation must be rejected");
    assert!(
        error.contains("http-text-stream HTTP requests"),
        "requests: {error}"
    );
}

/// The composed handler answers the module wire calls the sidecar dispatcher
/// makes, and a closed stream is terminal: the preparation is gone and the
/// recorder stops after the last pulled event.
#[tokio::test]
async fn composed_handler_serves_module_wire_calls() {
    let oracle = baseline::oracle(ORACLE);
    let providers = get(&oracle, "providers").clone();
    let scratch = Scratch::new("handler-wire");
    let transcript_path = scratch.0.join(format!("handler-wire-{SESSION}.jsonl"));
    let ok = obj(&[(
        "scripts",
        JsValue::Array(vec![obj(&[(
            "events",
            JsValue::Array(vec![
                parsed(r#"{"type":"message_start","messageId":"id"}"#),
                parsed(r#"{"type":"text_delta","text":"ok"}"#),
                parsed(r#"{"type":"usage","usage":{"inputTokens":3,"outputTokens":2}}"#),
                parsed(r#"{"type":"message_end","finishReason":"stop"}"#),
            ]),
        )])]),
    )]);
    let trace: Trace = Arc::default();
    let invoker = Arc::new(ScriptedInvoker {
        providers: providers.clone(),
        spec: ok,
        calls: Arc::default(),
    });
    let mut deps = RouterDeps::new(invoker);
    let events = trace.clone();
    deps.events = Arc::new(move |value| {
        events
            .lock()
            .unwrap()
            .push(obj(&[("kind", "router".into()), ("value", value)]));
    });
    deps.clock = Arc::new(|| 0.0);
    let snapshot = obj(&[(
        "config",
        obj(&[(
            "router",
            obj(&[
                ("enabled", JsValue::Bool(true)),
                (
                    "scenarios",
                    obj(&[(
                        "default",
                        parsed(r#"{"id":"a/m","provider":"a","model":"m"}"#),
                    )]),
                ),
            ]),
        )]),
    )]);
    let router = Arc::new(
        RouterRuntime::with_deps(RouterConfig::from_snapshot(&snapshot), deps)
            .expect("router runtime"),
    );
    let ids = AtomicUsize::new(0);
    let recorder = Recorder::with_sources(
        Arc::new(JsonlTranscriptStore::new()),
        TranscriptKey {
            project_root: PathBuf::from(PROJECT_ROOT),
            pilot_home: scratch.0.join("home"),
            session_id: SESSION.into(),
            transcript_path: transcript_path.clone(),
        },
        Arc::new(|| Ok(NOW.to_string())),
        Arc::new(move || Ok(format!("id-{}", ids.fetch_add(1, Ordering::SeqCst) + 1))),
    );
    let session: JsString = SESSION.into();
    let turn_id: JsString = "t1".into();
    recorder.start_turn(&session, &turn_id).unwrap();
    let handler = model_port::compose(
        router,
        obj(&[("providers", providers)]),
        Some(DurableRecorder {
            recorder: Arc::new(recorder),
            recording_session_id: None,
        }),
        RouterModelPortOptions::main_agent(),
        TurnContext {
            session_id: session.clone(),
            turn_id: turn_id.clone(),
            ..Default::default()
        },
    );
    let call = |operation: &str, request: Option<JsValue>| -> JsValue {
        let mut payload = JsObject::new();
        payload.insert("operation".into(), operation.into());
        payload.insert("preparationId".into(), "p1".into());
        if let Some(request) = request {
            payload.insert("request".into(), request);
        }
        obj(&[("payload", JsValue::Object(payload))])
    };
    let prepared = handler
        .call(call("prepare", Some(base_request())))
        .await
        .unwrap();
    let prepared = JsValue::Object(prepared);
    let prepared = get(&prepared, "prepared");
    assert_eq!(text(get(prepared, "provider")), "a");
    assert_eq!(text(get(prepared, "model")), "m");
    assert_eq!(
        get(get(prepared, "request"), "maxOutputTokens").as_number(),
        Some(64.0),
        "the provider capability caps the materialized request"
    );
    for (index, operation) in ["stream_next", "stream_next"].into_iter().enumerate() {
        let step = handler
            .call(call(operation, Some(base_request())))
            .await
            .unwrap();
        let step = JsValue::Object(step);
        assert_eq!(get(&step, "done"), &JsValue::Bool(false));
        assert_eq!(items(get(&step, "events")).len(), 1);
        assert_eq!(
            text(get(&items(get(&step, "events"))[0], "type")),
            if index == 0 {
                "message_start"
            } else {
                "text_delta"
            }
        );
    }
    let closed = handler.call(call("close_stream", None)).await.unwrap();
    assert_eq!(
        get(&JsValue::Object(closed), "closed"),
        &JsValue::Bool(true)
    );
    let error = handler
        .call(call("stream_next", None))
        .await
        .expect_err("the preparation is gone after close_stream");
    assert!(
        error.message.contains("Unknown sidecar model preparation"),
        "{error:?}"
    );
    let transcript = fs::read_to_string(&transcript_path).unwrap();
    let types: Vec<String> = transcript
        .lines()
        .map(|line| text(get(&parsed(line), "type")))
        .collect();
    // The model request is on disk before any stream event; close after two
    // pulled events writes nothing else (no step/turn completion).
    assert_eq!(
        types,
        vec![
            "turn_started",
            "step_started",
            "model_request",
            "model_stream_event",
            "model_stream_event",
        ]
    );
}
