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
use pilotdeck_gateway::storage::{StoreResult, TranscriptRead, TranscriptStore};
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
            AbortFlag, ManagedModelPolicy, RouterConfig, RouterDeps, RouterModelInvocation,
            RouterRuntime, StatsConfig,
        },
        stats::TokenStatsCollector,
    },
    storage::recorder::Recorder,
    storage::{JsonlTranscriptStore, TranscriptKey},
};
use pilotdeck_protocol::{JsObject, JsString, JsValue, parse_json, stringify_json};
use std::{
    fs,
    path::{Component, Path, PathBuf},
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
        options: &'a StreamOptions,
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
                // The TS provider generator suspends at every yield; the pull
                // that consumed the last delivered event is what resumes it.
                options.yield_boundary().await;
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
                        ("provider", prepared.provider.clone()),
                        ("model", prepared.model.clone()),
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
            // `record.mts` abort specs: `abortPre` / `abortAfterPulls` /
            // `abortAtMs` drive the same `context.abortSignal` controller the
            // dispatcher wires (`sidecarDefaultModuleDispatcher.ts:81,441`).
            let abort_pre = get(&spec, "abortPre") == &JsValue::Bool(true);
            let abort_after_pulls = get(&spec, "abortAfterPulls").as_number();
            let abort_at_ms = get(&spec, "abortAtMs").as_number();
            let turn_abort = (abort_pre || abort_after_pulls.is_some() || abort_at_ms.is_some())
                .then(AbortFlag::new);
            let mut stream = match execution
                .stream(StreamInput {
                    prepared: prepared.clone(),
                    context: context.clone(),
                    abort: turn_abort.clone(),
                })
                .await
            {
                Ok(stream) => stream,
                Err(error) => {
                    trace.lock().unwrap().push(error_entry(&error));
                    continue;
                }
            };
            if abort_pre {
                turn_abort.as_ref().expect("abort spec").abort();
            }
            if let Some(at_ms) = abort_at_ms {
                let flag = turn_abort.clone().expect("abort spec");
                tokio::spawn(async move {
                    tokio::time::sleep(std::time::Duration::from_millis(at_ms as u64)).await;
                    flag.abort();
                });
            }
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
                        if let Some(after) = abort_after_pulls
                            && pulled >= after
                        {
                            turn_abort.as_ref().expect("abort spec").abort();
                        }
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
    assert_eq!(cases.len(), 31);
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

/// Fix round 1: the turn's abort flag (`TurnContext::abort`,
/// `sidecarDefaultModuleDispatcher.ts:81,441` → `routerModelInvokerAdapter.ts:68`)
/// must interrupt a `stream_next` that is waiting on the provider — without
/// going through `close_stream`'s stream mutex. The waiting call rejects with
/// the abort error, the mock observes the dropped provider connection, and
/// the recorder never advances past the request.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn turn_cancel_interrupts_waiting_stream_next() {
    let oracle = baseline::oracle(ORACLE);
    let providers = get(&oracle, "providers").clone();
    let scratch = Scratch::new("turn-abort");
    let transcript_path = scratch.0.join(format!("turn-abort-{SESSION}.jsonl"));
    let exchange = Arc::new(Mutex::new(model_mock::Exchange {
        // The reply never sends a body event and never ends: the first pull
        // is waiting on the provider when the turn is cancelled.
        replies: vec![obj(&[
            ("chunks", JsValue::Array(vec![])),
            ("end", "hold".into()),
        ])],
        ..Default::default()
    }));
    let excluded = items(get(&oracle, "transportHeaders"))
        .iter()
        .map(text)
        .collect();
    let server = model_mock::start_mock(exchange.clone(), excluded, None);
    let mut configured = providers.as_object().cloned().unwrap();
    let mut provider = configured
        .get_key(&"a".into())
        .unwrap()
        .as_object()
        .unwrap()
        .clone();
    provider.insert(
        "url".into(),
        format!("{}/a/v1", server.origin).as_str().into(),
    );
    configured.insert("a".into(), JsValue::Object(provider));
    let model_config = obj(&[("providers", JsValue::Object(configured))]);
    let invoker: Arc<dyn RouterModelInvocation> = Arc::new(HttpRouterInvocation::new(
        Arc::new(ScriptedInvoker {
            providers: providers.clone(),
            spec: obj(&[("scripts", JsValue::Array(vec![]))]),
            calls: Arc::default(),
        }),
        Arc::new(HyperModelTransport::new()),
        model_config.clone(),
    ));
    let mut deps = RouterDeps::new(invoker);
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
    let abort = AbortFlag::new();
    let handler = model_port::compose(
        router,
        model_config,
        Some(DurableRecorder {
            recorder: Arc::new(recorder),
            recording_session_id: None,
        }),
        RouterModelPortOptions::main_agent(),
        TurnContext {
            session_id: session.clone(),
            turn_id: turn_id.clone(),
            abort: Some(abort.clone()),
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
    handler
        .call(call("prepare", Some(base_request())))
        .await
        .unwrap();
    let timer = {
        let flag = abort.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            flag.abort();
        })
    };
    let error = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        handler.call(call("stream_next", Some(base_request()))),
    )
    .await
    .expect("turn cancel must interrupt the waiting stream_next")
    .expect_err("the aborted provider wait must reject stream_next");
    assert_eq!(error.message, "This operation was aborted", "{error:?}");
    timer.await.expect("abort timer");
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let closed = exchange
                .lock()
                .unwrap()
                .requests
                .iter()
                .any(|request| request.get("clientClosed") == Some(&JsValue::Bool(true)));
            if closed {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("the cancelled turn must close the provider request");
    let transcript = fs::read_to_string(&transcript_path).unwrap();
    let types: Vec<String> = transcript
        .lines()
        .map(|line| text(get(&parsed(line), "type")))
        .collect();
    // The request was recorded before the body started; the aborted stream
    // never yields an event, so nothing else reaches the transcript.
    assert_eq!(types, vec!["turn_started", "step_started", "model_request"]);
}

/// Fix round 1: `RouterModelPortOptions::default()` is the main agent
/// (`routerModelInvokerAdapter.ts:33,41`: `options.isMainAgent ?? true` when
/// the caller omits the flag), so a composition that never sets it routes
/// with `isSubagent: false` — the derived `Default` regressed this to false.
#[tokio::test]
async fn default_options_compose_the_main_agent() {
    let oracle = baseline::oracle(ORACLE);
    let providers = get(&oracle, "providers").clone();
    let invoker: Arc<dyn RouterModelInvocation> = Arc::new(ScriptedInvoker {
        providers: providers.clone(),
        spec: obj(&[("scripts", JsValue::Array(vec![]))]),
        calls: Arc::default(),
    });
    let mut deps = RouterDeps::new(invoker);
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
    let session: JsString = SESSION.into();
    let model_port::ComposedPorts { execution, .. } = model_port::ports(
        router,
        obj(&[("providers", providers)]),
        None,
        RouterModelPortOptions::default(),
        &session,
    );
    assert!(RouterModelPortOptions::default().is_main_agent);
    let mut context = JsObject::new();
    context.insert("sessionId".into(), JsValue::String(session.clone()));
    context.insert("turnId".into(), "t1".into());
    context.insert("runId".into(), "t1".into());
    let prepared = execution
        .prepare(PrepareInput {
            request: base_request(),
            context,
        })
        .await
        .expect("prepare");
    let decision = prepared.opaque.expect("router decision");
    assert_eq!(get(&decision, "isSubagent"), &JsValue::Bool(false));
}

/// A [`TranscriptStore`] that parks chosen appends: `gate(type)` blocks the
/// next append of that type inside the record's blocking task — after it has
/// signalled `arrived` — until the test releases it, pinning down exactly
/// where a record stands when its pull is cancelled. One-shot: each gate
/// parks one append.
struct GatedStore {
    inner: Arc<JsonlTranscriptStore>,
    log: Trace,
    tag: String,
    arrived: tokio::sync::mpsc::UnboundedSender<String>,
    gates: Mutex<Vec<(String, std::sync::mpsc::Receiver<()>)>>,
}
impl GatedStore {
    fn gate(&self, ty: &str) -> std::sync::mpsc::Sender<()> {
        let (release, gate) = std::sync::mpsc::channel();
        self.gates.lock().unwrap().push((ty.into(), gate));
        release
    }
}
impl TranscriptStore for GatedStore {
    fn append(&self, key: &TranscriptKey, entry: &JsValue) -> StoreResult<()> {
        let ty = text(get(entry, "type"));
        self.log.lock().unwrap().push(entry.clone());
        let _ = self.arrived.send(format!("{}:{}", self.tag, ty));
        let gated = {
            let mut gates = self.gates.lock().unwrap();
            gates
                .iter()
                .position(|(gated, _)| gated == &ty)
                .map(|index| gates.remove(index))
        };
        if let Some((_, release)) = gated {
            // Parks the record's blocking task until the test lets it
            // through; dropping the release sender fails the append instead.
            match release.recv() {
                Ok(()) => {}
                Err(_) => return Err(std::io::Error::other("gated record failure").into()),
            }
        }
        self.inner.append(key, entry)
    }
    fn read(&self, key: &TranscriptKey, max_bytes: Option<u64>) -> StoreResult<TranscriptRead> {
        self.inner.read(key, max_bytes)
    }
}

/// The test side of a [`GatedStore`]: one shared arrival stream.
struct Gate {
    arrived: tokio::sync::mpsc::UnboundedReceiver<String>,
}
impl Gate {
    async fn wait_for(&mut self, tag: &str, ty: &str) {
        let want = format!("{tag}:{ty}");
        tokio::time::timeout(std::time::Duration::from_secs(10), async {
            while self.arrived.recv().await.expect("arrival channel") != want {}
        })
        .await
        .unwrap_or_else(|_| panic!("record {want} never arrived"));
    }
}

/// The replay harness's recorder over an injectable store.
fn recorder_at(
    store: Arc<dyn TranscriptStore>,
    scratch: &Path,
    name: &str,
) -> (Arc<Recorder>, PathBuf) {
    let transcript_path = scratch.join(format!("{name}-{SESSION}.jsonl"));
    let ids = AtomicUsize::new(0);
    (
        Arc::new(Recorder::with_sources(
            store,
            TranscriptKey {
                project_root: PathBuf::from(PROJECT_ROOT),
                pilot_home: scratch.join("home"),
                session_id: SESSION.into(),
                transcript_path: transcript_path.clone(),
            },
            Arc::new(|| Ok(NOW.to_string())),
            Arc::new(move || Ok(format!("id-{}", ids.fetch_add(1, Ordering::SeqCst) + 1))),
        )),
        transcript_path,
    )
}

/// `{sessionId, turnId, runId}`, like the TS driver's turn context.
fn session_context(turn_id: &str) -> JsObject {
    let mut context = JsObject::new();
    context.insert("sessionId".into(), SESSION.into());
    context.insert("turnId".into(), turn_id.into());
    context.insert("runId".into(), turn_id.into());
    context
}

/// The replay harness's router wiring: the `a/m` scenario, no fallback, the
/// given invoker, router events collected into `trace`.
fn test_router(invoker: Arc<dyn RouterModelInvocation>, trace: &Trace) -> Arc<RouterRuntime> {
    test_router_with(
        invoker, trace, None, // no injected stats collector: `observeRequest` is a no-op
    )
}

/// Like [`test_router`], with an injected stats collector when the test needs
/// to observe `observeRequest` (`TokenStatsCollector` over a per-test file,
/// never the default `~/.pilotdeck` location).
fn test_router_with(
    invoker: Arc<dyn RouterModelInvocation>,
    trace: &Trace,
    stats: Option<Arc<Mutex<TokenStatsCollector>>>,
) -> Arc<RouterRuntime> {
    let mut deps = RouterDeps::new(invoker);
    let events = trace.clone();
    deps.events = Arc::new(move |value| {
        events
            .lock()
            .unwrap()
            .push(obj(&[("kind", "router".into()), ("value", value)]));
    });
    deps.clock = Arc::new(|| 0.0);
    deps.stats = stats;
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
    Arc::new(
        RouterRuntime::with_deps(RouterConfig::from_snapshot(&snapshot), deps)
            .expect("router runtime"),
    )
}

/// The replay harness's HTTP wiring for the focused tests: provider `a` at
/// the loopback mock origin, the durable recorder, a test-built router.
fn http_ports_with(
    providers: &JsValue,
    origin: &str,
    recorder: Arc<Recorder>,
    router: Arc<RouterRuntime>,
) -> model_port::ComposedPorts {
    let mut configured = providers.as_object().cloned().unwrap();
    let mut provider = configured
        .get_key(&"a".into())
        .unwrap()
        .as_object()
        .unwrap()
        .clone();
    provider.insert("url".into(), format!("{origin}/a/v1").as_str().into());
    configured.insert("a".into(), JsValue::Object(provider));
    let model_config = obj(&[("providers", JsValue::Object(configured))]);
    let session: JsString = SESSION.into();
    model_port::ports(
        router,
        model_config,
        Some(DurableRecorder {
            recorder,
            recording_session_id: None,
        }),
        RouterModelPortOptions::main_agent(),
        &session,
    )
}

/// [`http_ports_with`] around the replay harness's default router.
fn http_ports(
    providers: &JsValue,
    origin: &str,
    recorder: Arc<Recorder>,
    trace: &Trace,
) -> model_port::ComposedPorts {
    let mut configured = providers.as_object().cloned().unwrap();
    let mut provider = configured
        .get_key(&"a".into())
        .unwrap()
        .as_object()
        .unwrap()
        .clone();
    provider.insert("url".into(), format!("{origin}/a/v1").as_str().into());
    configured.insert("a".into(), JsValue::Object(provider));
    let model_config = obj(&[("providers", JsValue::Object(configured))]);
    let invoker: Arc<dyn RouterModelInvocation> = Arc::new(HttpRouterInvocation::new(
        Arc::new(ScriptedInvoker {
            providers: providers.clone(),
            spec: obj(&[("scripts", JsValue::Array(vec![]))]),
            calls: Arc::default(),
        }),
        Arc::new(HyperModelTransport::new()),
        model_config.clone(),
    ));
    http_ports_with(providers, origin, recorder, test_router(invoker, trace))
}

async fn wait_client_closed(exchange: &Arc<Mutex<model_mock::Exchange>>, message: &str) {
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let closed = exchange
                .lock()
                .unwrap()
                .requests
                .iter()
                .any(|request| request.get("clientClosed") == Some(&JsValue::Bool(true)));
            if closed {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("{message}"));
}

fn assert_no_execute_failed(trace: &Trace) {
    let trace = trace.lock().unwrap();
    assert!(
        trace
            .iter()
            .all(|entry| !json(entry).contains("pilotdeck_router_execute_failed")),
        "the aborted body must not publish terminal side effects: {trace:?}"
    );
}

fn transcript_types(path: &Path) -> Vec<String> {
    fs::read_to_string(path)
        .unwrap()
        .lines()
        .map(|line| text(get(&parsed(line), "type")))
        .collect()
}

/// Fix round 2 (codex #1): the turn abort must reach the provider without a
/// consumer pull. TS registers the abort listener on the response body when
/// the stream starts (`streamModel.ts`'s `cancelReader`), so a turn cancelled
/// while the consumer sits between pulls still closes the provider request —
/// the forwarder task in `RouterModelStream` forwards it the moment it fires.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn turn_cancel_between_pulls_closes_the_provider() {
    let oracle = baseline::oracle(ORACLE);
    let providers = get(&oracle, "providers").clone();
    let scratch = Scratch::new("between-pulls");
    // The `http-text-stream` reply's first two chunks (the `message_start`
    // frame, then `content_block_start` + `content_block_delta`); the reply
    // then holds the connection open, so no further body events arrive.
    let http_case = items(get(&oracle, "cases"))
        .iter()
        .find(|case| case_name(case) == "http-text-stream")
        .unwrap()
        .clone();
    let frames = items(get(
        &items(get(get(&http_case, "spec"), "replies"))[0],
        "chunks",
    ))
    .to_vec();
    let exchange = Arc::new(Mutex::new(model_mock::Exchange {
        replies: vec![obj(&[
            (
                "chunks",
                JsValue::Array(
                    frames
                        .iter()
                        .take(2)
                        .map(|frame| obj(&[("text", frame.clone())]))
                        .collect(),
                ),
            ),
            ("end", "hold".into()),
        ])],
        ..Default::default()
    }));
    let excluded = items(get(&oracle, "transportHeaders"))
        .iter()
        .map(text)
        .collect();
    let server = model_mock::start_mock(exchange.clone(), excluded, None);
    let (recorder, transcript_path) = recorder_at(
        Arc::new(JsonlTranscriptStore::new()),
        &scratch.0,
        "between-pulls",
    );
    let trace: Trace = Arc::default();
    let model_port::ComposedPorts { execution, .. } =
        http_ports(&providers, &server.origin, recorder.clone(), &trace);
    let session: JsString = SESSION.into();
    let turn_id: JsString = "t1".into();
    recorder.start_turn(&session, &turn_id).unwrap();
    let prepared = execution
        .prepare(PrepareInput {
            request: base_request(),
            context: session_context("t1"),
        })
        .await
        .expect("prepare");
    let abort = AbortFlag::new();
    let mut stream = execution
        .stream(StreamInput {
            prepared,
            context: session_context("t1"),
            abort: Some(abort.clone()),
        })
        .await
        .expect("stream");
    let first = stream.next().await.expect("pull").expect("event");
    assert_eq!(text(get(&first, "type")), "request_started");
    // No further pull and no close: the cancelled turn alone must close the
    // provider request.
    abort.abort();
    wait_client_closed(
        &exchange,
        "a turn cancelled between pulls must close the provider request",
    )
    .await;
    assert_no_execute_failed(&trace);
    assert_eq!(
        transcript_types(&transcript_path),
        vec![
            "turn_started",
            "step_started",
            "model_request",
            "model_stream_event",
        ],
        "only the pulled event is on disk"
    );
    stream.close().await.expect("close");
}

/// Fix round 2 (codex #1, recorder window): the turn abort must reach the
/// provider even while the outer durable pull sits between its queue-take
/// and the event's record. The gated store parks the pull exactly there; the
/// parked record still lands (TS writes it synchronously inside the pull,
/// `durableModelInvokerPort.ts`), and the pull delivers its event once the
/// record completes.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn turn_cancel_during_record_await_closes_the_provider() {
    let oracle = baseline::oracle(ORACLE);
    let providers = get(&oracle, "providers").clone();
    let scratch = Scratch::new("record-await");
    let http_case = items(get(&oracle, "cases"))
        .iter()
        .find(|case| case_name(case) == "http-text-stream")
        .unwrap()
        .clone();
    let frames = items(get(
        &items(get(get(&http_case, "spec"), "replies"))[0],
        "chunks",
    ))
    .to_vec();
    let exchange = Arc::new(Mutex::new(model_mock::Exchange {
        replies: vec![obj(&[
            (
                "chunks",
                JsValue::Array(
                    frames
                        .iter()
                        .take(2)
                        .map(|frame| obj(&[("text", frame.clone())]))
                        .collect(),
                ),
            ),
            ("end", "hold".into()),
        ])],
        ..Default::default()
    }));
    let excluded = items(get(&oracle, "transportHeaders"))
        .iter()
        .map(text)
        .collect();
    let server = model_mock::start_mock(exchange.clone(), excluded, None);
    let (arrived, gate_rx) = tokio::sync::mpsc::unbounded_channel();
    let store = Arc::new(GatedStore {
        inner: Arc::new(JsonlTranscriptStore::new()),
        log: Arc::default(),
        tag: "main".into(),
        arrived,
        gates: Mutex::new(Vec::new()),
    });
    // Two one-shot gates on the event records: the first parks pull 1's own
    // record (released right away), the second parks pull 2's.
    let releases = [
        store.gate("model_stream_event"),
        store.gate("model_stream_event"),
    ];
    let mut gate = Gate { arrived: gate_rx };
    let (recorder, transcript_path) = recorder_at(store.clone(), &scratch.0, "record-await");
    let trace: Trace = Arc::default();
    let model_port::ComposedPorts { execution, .. } =
        http_ports(&providers, &server.origin, recorder.clone(), &trace);
    let session: JsString = SESSION.into();
    let turn_id: JsString = "t1".into();
    recorder.start_turn(&session, &turn_id).unwrap();
    let prepared = execution
        .prepare(PrepareInput {
            request: base_request(),
            context: session_context("t1"),
        })
        .await
        .expect("prepare");
    let abort = AbortFlag::new();
    let mut stream = execution
        .stream(StreamInput {
            prepared,
            context: session_context("t1"),
            abort: Some(abort.clone()),
        })
        .await
        .expect("stream");
    let mut first = stream.next();
    tokio::select! {
        result = &mut first => panic!("pull 1 must park at its record gate, got {result:?}"),
        () = gate.wait_for("main", "model_stream_event") => {}
    }
    releases[0].send(()).unwrap();
    let first_event = first.await.expect("pull").expect("event");
    assert_eq!(text(get(&first_event, "type")), "request_started");
    // Pull 2 takes its event from the queue and parks on the record.
    let mut second = stream.next();
    tokio::select! {
        result = &mut second => panic!("pull 2 must park at its record gate, got {result:?}"),
        () = gate.wait_for("main", "model_stream_event") => {}
    }
    // The turn cancels while the pull sits between its queue-take and the
    // record: no further pull, no close.
    abort.abort();
    wait_client_closed(
        &exchange,
        "a turn cancelled during a record await must close the provider request",
    )
    .await;
    assert_no_execute_failed(&trace);
    // Let the parked record complete: the pull still delivers its event, and
    // the record landed exactly once.
    releases[1].send(()).unwrap();
    let second_event = second.await.expect("pull").expect("event");
    assert_eq!(text(get(&second_event, "type")), "message_start");
    assert_eq!(
        transcript_types(&transcript_path),
        vec![
            "turn_started",
            "step_started",
            "model_request",
            "model_stream_event",
            "model_stream_event",
        ],
        "the two pulled events are on disk in pull order"
    );
    stream.close().await.expect("close");
}

/// Fix round 2 (codex #2): a pull cancelled while its request record is in
/// flight must not let the next pull deliver events before the record lands.
/// The record runs in its own task and the stream only advances once its
/// completion is observed, so the retried pull waits for it first — TS
/// records the request synchronously inside the first pull
/// (`durableModelInvokerPort.ts`), so no event may precede it on disk, and
/// the write happens exactly once no matter how many pulls were cancelled.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancelled_pull_waits_for_the_request_record() {
    let oracle = baseline::oracle(ORACLE);
    let providers = get(&oracle, "providers").clone();
    let scratch = Scratch::new("request-record");
    let (arrived, gate_rx) = tokio::sync::mpsc::unbounded_channel();
    let store = Arc::new(GatedStore {
        inner: Arc::new(JsonlTranscriptStore::new()),
        log: Arc::default(),
        tag: "main".into(),
        arrived,
        gates: Mutex::new(Vec::new()),
    });
    let release = store.gate("model_request");
    let mut gate = Gate { arrived: gate_rx };
    let (recorder, transcript_path) = recorder_at(store.clone(), &scratch.0, "request-record");
    let trace: Trace = Arc::default();
    let script = obj(&[(
        "scripts",
        JsValue::Array(vec![obj(&[(
            "events",
            JsValue::Array(vec![
                parsed(r#"{"type":"message_start","messageId":"id"}"#),
                parsed(r#"{"type":"text_delta","text":"ok"}"#),
            ]),
        )])]),
    )]);
    let router = test_router(
        Arc::new(ScriptedInvoker {
            providers: providers.clone(),
            spec: script,
            calls: Arc::default(),
        }),
        &trace,
    );
    let session: JsString = SESSION.into();
    let turn_id: JsString = "t1".into();
    recorder.start_turn(&session, &turn_id).unwrap();
    let model_port::ComposedPorts { execution, .. } = model_port::ports(
        router,
        obj(&[("providers", providers.clone())]),
        Some(DurableRecorder {
            recorder: recorder.clone(),
            recording_session_id: None,
        }),
        RouterModelPortOptions::main_agent(),
        &session,
    );
    let prepared = execution
        .prepare(PrepareInput {
            request: base_request(),
            context: session_context("t1"),
        })
        .await
        .expect("prepare");
    let mut stream = execution
        .stream(StreamInput {
            prepared,
            context: session_context("t1"),
            abort: None,
        })
        .await
        .expect("stream");
    // Pull 1 parks on the gated request record, then the pull is cancelled.
    let mut first = stream.next();
    tokio::select! {
        result = &mut first => panic!("pull 1 must park at its record gate, got {result:?}"),
        () = gate.wait_for("main", "model_request") => {}
    }
    drop(first);
    // The retried pull must wait for the in-flight record instead of
    // delivering events first.
    let mut retry = stream.next();
    let () = tokio::select! {
        result = &mut retry => panic!(
            "the retried pull must wait for the request record, got {result:?}"
        ),
        _ = tokio::time::sleep(std::time::Duration::from_millis(100)) => {}
    };
    release.send(()).unwrap();
    let event = retry.await.expect("pull").expect("event");
    assert_eq!(text(get(&event, "type")), "message_start");
    stream.close().await.expect("close");
    let log = store.log.lock().unwrap();
    let requests = log
        .iter()
        .filter(|entry| text(get(entry, "type")) == "model_request")
        .count();
    let logged = log
        .iter()
        .map(|entry| text(get(entry, "type")))
        .collect::<Vec<_>>();
    assert_eq!(
        requests, 1,
        "the request record lands exactly once: {logged:?}"
    );
    assert_eq!(
        transcript_types(&transcript_path),
        vec![
            "turn_started",
            "step_started",
            "model_request",
            "model_stream_event",
        ],
        "the request is on disk before any event, TS order"
    );
}

/// Fix round 2 (codex #2, failure half): a pull cancelled while its request
/// record is in flight must not swallow the record's failure. The record
/// runs in its own task, so its error — even when the pull that started it
/// was cancelled in between — surfaces on the retried pull: the stream never
/// proceeds to events without a request on disk.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancelled_pull_surfaces_the_request_record_failure() {
    let oracle = baseline::oracle(ORACLE);
    let providers = get(&oracle, "providers").clone();
    let scratch = Scratch::new("record-failure");
    let (arrived, gate_rx) = tokio::sync::mpsc::unbounded_channel();
    let store = Arc::new(GatedStore {
        inner: Arc::new(JsonlTranscriptStore::new()),
        log: Arc::default(),
        tag: "main".into(),
        arrived,
        gates: Mutex::new(Vec::new()),
    });
    let release = store.gate("model_request");
    let mut gate = Gate { arrived: gate_rx };
    let (recorder, transcript_path) = recorder_at(store.clone(), &scratch.0, "record-failure");
    let trace: Trace = Arc::default();
    let script = obj(&[(
        "scripts",
        JsValue::Array(vec![obj(&[(
            "events",
            JsValue::Array(vec![parsed(r#"{"type":"message_start","messageId":"id"}"#)]),
        )])]),
    )]);
    let router = test_router(
        Arc::new(ScriptedInvoker {
            providers: providers.clone(),
            spec: script,
            calls: Arc::default(),
        }),
        &trace,
    );
    let session: JsString = SESSION.into();
    let turn_id: JsString = "t1".into();
    recorder.start_turn(&session, &turn_id).unwrap();
    let model_port::ComposedPorts { execution, .. } = model_port::ports(
        router,
        obj(&[("providers", providers.clone())]),
        Some(DurableRecorder {
            recorder: recorder.clone(),
            recording_session_id: None,
        }),
        RouterModelPortOptions::main_agent(),
        &session,
    );
    let prepared = execution
        .prepare(PrepareInput {
            request: base_request(),
            context: session_context("t1"),
        })
        .await
        .expect("prepare");
    let mut stream = execution
        .stream(StreamInput {
            prepared,
            context: session_context("t1"),
            abort: None,
        })
        .await
        .expect("stream");
    // Pull 1 parks on the gated request record, then the pull is cancelled
    // and the record fails.
    let mut first = stream.next();
    tokio::select! {
        result = &mut first => panic!("pull 1 must park at its record gate, got {result:?}"),
        () = gate.wait_for("main", "model_request") => {}
    }
    drop(first);
    drop(release);
    let error = stream
        .next()
        .await
        .expect_err("the retried pull must surface the request record failure");
    assert!(
        error
            .message
            .to_utf8()
            .unwrap()
            .contains("gated record failure"),
        "{error:?}"
    );
    stream.close().await.expect("close");
    // The failed record still wrote everything before it; the request and
    // no events are on disk.
    assert_eq!(
        transcript_types(&transcript_path),
        vec!["turn_started", "step_started"],
        "no events may be recorded without the request"
    );
}

/// Fix round 2 (codex #2, event half): a pull cancelled between the inner
/// queue-take and its event record must neither lose the event nor skip its
/// record. The event stays held in the stream until its record is observed
/// complete, so the retried pull delivers the same event — never the next
/// one, never a duplicate record.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancelled_pull_replays_the_held_event() {
    let oracle = baseline::oracle(ORACLE);
    let providers = get(&oracle, "providers").clone();
    let scratch = Scratch::new("held-event");
    let (arrived, gate_rx) = tokio::sync::mpsc::unbounded_channel();
    let store = Arc::new(GatedStore {
        inner: Arc::new(JsonlTranscriptStore::new()),
        log: Arc::default(),
        tag: "main".into(),
        arrived,
        gates: Mutex::new(Vec::new()),
    });
    let release = store.gate("model_stream_event");
    let mut gate = Gate { arrived: gate_rx };
    let (recorder, transcript_path) = recorder_at(store.clone(), &scratch.0, "held-event");
    let trace: Trace = Arc::default();
    let script = obj(&[(
        "scripts",
        JsValue::Array(vec![obj(&[(
            "events",
            JsValue::Array(vec![
                parsed(r#"{"type":"message_start","messageId":"id"}"#),
                parsed(r#"{"type":"text_delta","text":"ok"}"#),
            ]),
        )])]),
    )]);
    let router = test_router(
        Arc::new(ScriptedInvoker {
            providers: providers.clone(),
            spec: script,
            calls: Arc::default(),
        }),
        &trace,
    );
    let session: JsString = SESSION.into();
    let turn_id: JsString = "t1".into();
    recorder.start_turn(&session, &turn_id).unwrap();
    let model_port::ComposedPorts { execution, .. } = model_port::ports(
        router,
        obj(&[("providers", providers.clone())]),
        Some(DurableRecorder {
            recorder: recorder.clone(),
            recording_session_id: None,
        }),
        RouterModelPortOptions::main_agent(),
        &session,
    );
    let prepared = execution
        .prepare(PrepareInput {
            request: base_request(),
            context: session_context("t1"),
        })
        .await
        .expect("prepare");
    let mut stream = execution
        .stream(StreamInput {
            prepared,
            context: session_context("t1"),
            abort: None,
        })
        .await
        .expect("stream");
    // Pull 1 takes `message_start` from the queue, parks on its record, then
    // the pull is cancelled.
    let mut first = stream.next();
    tokio::select! {
        result = &mut first => panic!("pull 1 must park at its record gate, got {result:?}"),
        () = gate.wait_for("main", "model_stream_event") => {}
    }
    drop(first);
    release.send(()).unwrap();
    let first_event = stream.next().await.expect("pull").expect("event");
    assert_eq!(
        text(get(&first_event, "type")),
        "message_start",
        "the retried pull delivers the held event, not the next one"
    );
    let second_event = stream.next().await.expect("pull").expect("event");
    assert_eq!(text(get(&second_event, "type")), "text_delta");
    stream.close().await.expect("close");
    let log = store.log.lock().unwrap();
    let events = log
        .iter()
        .filter(|entry| text(get(entry, "type")) == "model_stream_event")
        .map(|entry| text(get(get(entry, "event"), "type")))
        .collect::<Vec<_>>();
    assert_eq!(
        events,
        vec!["message_start", "text_delta"],
        "each pulled event is recorded exactly once, in order"
    );
    assert_eq!(
        transcript_types(&transcript_path),
        vec![
            "turn_started",
            "step_started",
            "model_request",
            "model_stream_event",
            "model_stream_event",
        ],
        "the two pulled events are on disk in pull order"
    );
}

/// An enabled `TokenStatsCollector` over a per-test file (never the default
/// `~/.pilotdeck` location), so `observeRequest` is observable.
fn stats_collector(path: &Path) -> Arc<Mutex<TokenStatsCollector>> {
    Arc::new(Mutex::new(TokenStatsCollector::new(
        Some(&StatsConfig {
            enabled: true,
            model_pricing: None,
            file_path: Some(path.to_path_buf()),
            retention_ms: None,
            baseline_model: None,
        }),
        None,
    )))
}

/// The scope of the session usage snapshot: "session" once `observeRequest`
/// recorded the session's first request, "project" while it never ran.
fn stats_scope(router: &RouterRuntime) -> String {
    let session: JsString = SESSION.into();
    text(get(&router.usage_snapshot(Some(&session)), "scope"))
}

/// Fix round 3 (codex #1): the turn cancel must not cross the Router's
/// terminal boundary. The contentless failing attempt already drained its
/// pending events; the consumer pulled both and cancelled the turn. TS keeps
/// the generator suspended at its yield — the cancel only tears the provider
/// connection down (`streamModel.ts`'s `cancelReader`), `execute_failed` and
/// `observeRequest` stay behind the pulls that never come
/// (`RouterRuntime.ts:962` behind the yields of `:873-877`), and the later
/// close discards the rest.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn turn_cancel_keeps_terminal_failure_behind_the_pulls() {
    let oracle = baseline::oracle(ORACLE);
    let providers = get(&oracle, "providers").clone();
    let scratch = Scratch::new("terminal-abort");
    let http_case = items(get(&oracle, "cases"))
        .iter()
        .find(|case| case_name(case) == "http-text-stream")
        .unwrap()
        .clone();
    let wire_start = items(get(
        &items(get(get(&http_case, "spec"), "replies"))[0],
        "chunks",
    ))[0]
        .clone();
    // `event: error` with a non-retryable `error.type`: the anthropic
    // normalizer keeps `retryable: false`, so the attempt fails terminally
    // without fallback eligibility.
    let wire_error = "event: error\ndata: {\"type\":\"error\",\"error\":{\"type\":\"provider_error\",\"message\":\"boom\"}}\n\n";
    let exchange = Arc::new(Mutex::new(model_mock::Exchange {
        replies: vec![obj(&[(
            "chunks",
            JsValue::Array(vec![
                obj(&[("text", wire_start)]),
                obj(&[("text", wire_error.into())]),
            ]),
        )])],
        ..Default::default()
    }));
    let excluded = items(get(&oracle, "transportHeaders"))
        .iter()
        .map(text)
        .collect();
    let server = model_mock::start_mock(exchange.clone(), excluded, None);
    let (recorder, transcript_path) = recorder_at(
        Arc::new(JsonlTranscriptStore::new()),
        &scratch.0,
        "terminal-abort",
    );
    let trace: Trace = Arc::default();
    let mut configured = providers.as_object().cloned().unwrap();
    let mut provider = configured
        .get_key(&"a".into())
        .unwrap()
        .as_object()
        .unwrap()
        .clone();
    provider.insert(
        "url".into(),
        format!("{}/a/v1", server.origin).as_str().into(),
    );
    configured.insert("a".into(), JsValue::Object(provider));
    let model_config = obj(&[("providers", JsValue::Object(configured))]);
    let invoker: Arc<dyn RouterModelInvocation> = Arc::new(HttpRouterInvocation::new(
        Arc::new(ScriptedInvoker {
            providers: providers.clone(),
            spec: obj(&[("scripts", JsValue::Array(vec![]))]),
            calls: Arc::default(),
        }),
        Arc::new(HyperModelTransport::new()),
        model_config.clone(),
    ));
    let router = test_router_with(
        invoker,
        &trace,
        Some(stats_collector(&scratch.0.join("stats.jsonl"))),
    );
    let model_port::ComposedPorts { execution, .. } =
        http_ports_with(&providers, &server.origin, recorder.clone(), router.clone());
    let session: JsString = SESSION.into();
    let turn_id: JsString = "t1".into();
    recorder.start_turn(&session, &turn_id).unwrap();
    let prepared = execution
        .prepare(PrepareInput {
            request: base_request(),
            context: session_context("t1"),
        })
        .await
        .expect("prepare");
    let abort = AbortFlag::new();
    let mut stream = execution
        .stream(StreamInput {
            prepared,
            context: session_context("t1"),
            abort: Some(abort.clone()),
        })
        .await
        .expect("stream");
    let first = stream.next().await.expect("pull").expect("event");
    assert_eq!(text(get(&first, "type")), "request_started");
    let second = stream.next().await.expect("pull").expect("event");
    assert_eq!(text(get(&second, "type")), "message_start");
    // No further pull and no close: the cancelled turn alone must not
    // publish the terminal failure or the request stats.
    abort.abort();
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    assert_no_execute_failed(&trace);
    assert_eq!(
        stats_scope(&router),
        "project",
        "observeRequest must not run without the consuming pulls"
    );
    assert_eq!(
        exchange.lock().unwrap().requests.len(),
        1,
        "one provider request for the failed attempt"
    );
    assert_eq!(
        transcript_types(&transcript_path),
        vec![
            "turn_started",
            "step_started",
            "model_request",
            "model_stream_event",
            "model_stream_event",
        ],
        "only the two pulled events are on disk"
    );
    stream.close().await.expect("close");
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    assert_no_execute_failed(&trace);
    assert_eq!(stats_scope(&router), "project", "close discards the rest");
    assert_eq!(
        transcript_types(&transcript_path),
        vec![
            "turn_started",
            "step_started",
            "model_request",
            "model_stream_event",
            "model_stream_event",
        ],
        "close records nothing new"
    );
}

/// Fix round 3 (codex #1, record window): the same contentless terminal
/// failure, with the first pulled event's record still in flight when the
/// turn is cancelled. The record is what the durable wrapper writes inside
/// the pull (`durableModelInvokerPort.ts`); the failure publication must not
/// overtake it — it stays behind the pull that is still parked on the record,
/// and the later close discards it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn turn_cancel_during_first_record_keeps_terminal_failure_gated() {
    let oracle = baseline::oracle(ORACLE);
    let providers = get(&oracle, "providers").clone();
    let scratch = Scratch::new("terminal-record");
    let http_case = items(get(&oracle, "cases"))
        .iter()
        .find(|case| case_name(case) == "http-text-stream")
        .unwrap()
        .clone();
    let wire_start = items(get(
        &items(get(get(&http_case, "spec"), "replies"))[0],
        "chunks",
    ))[0]
        .clone();
    let wire_error = "event: error\ndata: {\"type\":\"error\",\"error\":{\"type\":\"provider_error\",\"message\":\"boom\"}}\n\n";
    let exchange = Arc::new(Mutex::new(model_mock::Exchange {
        replies: vec![obj(&[(
            "chunks",
            JsValue::Array(vec![
                obj(&[("text", wire_start)]),
                obj(&[("text", wire_error.into())]),
            ]),
        )])],
        ..Default::default()
    }));
    let excluded = items(get(&oracle, "transportHeaders"))
        .iter()
        .map(text)
        .collect();
    let server = model_mock::start_mock(exchange.clone(), excluded, None);
    let (arrived, gate_rx) = tokio::sync::mpsc::unbounded_channel();
    let store = Arc::new(GatedStore {
        inner: Arc::new(JsonlTranscriptStore::new()),
        log: Arc::default(),
        tag: "main".into(),
        arrived,
        gates: Mutex::new(Vec::new()),
    });
    let release = store.gate("model_stream_event");
    let mut gate = Gate { arrived: gate_rx };
    let (recorder, transcript_path) = recorder_at(store.clone(), &scratch.0, "terminal-record");
    let trace: Trace = Arc::default();
    let mut configured = providers.as_object().cloned().unwrap();
    let mut provider = configured
        .get_key(&"a".into())
        .unwrap()
        .as_object()
        .unwrap()
        .clone();
    provider.insert(
        "url".into(),
        format!("{}/a/v1", server.origin).as_str().into(),
    );
    configured.insert("a".into(), JsValue::Object(provider));
    let model_config = obj(&[("providers", JsValue::Object(configured))]);
    let invoker: Arc<dyn RouterModelInvocation> = Arc::new(HttpRouterInvocation::new(
        Arc::new(ScriptedInvoker {
            providers: providers.clone(),
            spec: obj(&[("scripts", JsValue::Array(vec![]))]),
            calls: Arc::default(),
        }),
        Arc::new(HyperModelTransport::new()),
        model_config.clone(),
    ));
    let router = test_router_with(
        invoker,
        &trace,
        Some(stats_collector(&scratch.0.join("stats.jsonl"))),
    );
    let model_port::ComposedPorts { execution, .. } =
        http_ports_with(&providers, &server.origin, recorder.clone(), router.clone());
    let session: JsString = SESSION.into();
    let turn_id: JsString = "t1".into();
    recorder.start_turn(&session, &turn_id).unwrap();
    let prepared = execution
        .prepare(PrepareInput {
            request: base_request(),
            context: session_context("t1"),
        })
        .await
        .expect("prepare");
    let abort = AbortFlag::new();
    let mut stream = execution
        .stream(StreamInput {
            prepared,
            context: session_context("t1"),
            abort: Some(abort.clone()),
        })
        .await
        .expect("stream");
    // Pull 1 parks on the gated record of the first pulled event.
    let mut first = stream.next();
    tokio::select! {
        result = &mut first => panic!("pull 1 must park at its record gate, got {result:?}"),
        () = gate.wait_for("main", "model_stream_event") => {}
    }
    // The turn cancels while that record is still in flight: no further
    // pull, no close, and the failure publication must not overtake the
    // record window.
    abort.abort();
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    assert_no_execute_failed(&trace);
    assert_eq!(stats_scope(&router), "project");
    // Let the parked record complete: the pull still delivers its event.
    release.send(()).unwrap();
    let first_event = first.await.expect("pull").expect("event");
    assert_eq!(text(get(&first_event, "type")), "request_started");
    stream.close().await.expect("close");
    assert_no_execute_failed(&trace);
    assert_eq!(stats_scope(&router), "project");
    assert_eq!(
        transcript_types(&transcript_path),
        vec![
            "turn_started",
            "step_started",
            "model_request",
            "model_stream_event",
        ],
        "the parked record landed exactly once, TS order"
    );
}

/// Fix round 3 (the other side of the boundary): the terminal boundary still
/// opens on pulls after the cancel — the suspended generator resumes where it
/// stopped, and the terminal publication follows the pull that consumed the
/// queued events (`RouterRuntime.ts:962` behind the yields of `:873-877`;
/// TS also re-yields the non-error buffered events, `:993-998`).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pulls_after_the_cancel_still_publish_the_terminal_failure() {
    let oracle = baseline::oracle(ORACLE);
    let providers = get(&oracle, "providers").clone();
    let scratch = Scratch::new("terminal-pull");
    let (recorder, transcript_path) = recorder_at(
        Arc::new(JsonlTranscriptStore::new()),
        &scratch.0,
        "terminal-pull",
    );
    let trace: Trace = Arc::default();
    let script = obj(&[(
        "scripts",
        JsValue::Array(vec![obj(&[(
            "events",
            JsValue::Array(vec![
                parsed(r#"{"type":"message_start","messageId":"id"}"#),
                parsed(
                    r#"{"type":"error","error":{"provider":"a","protocol":"anthropic","code":"server_error","message":"failed","retryable":false}}"#,
                ),
            ]),
        )])]),
    )]);
    let router = test_router_with(
        Arc::new(ScriptedInvoker {
            providers: providers.clone(),
            spec: script,
            calls: Arc::default(),
        }),
        &trace,
        Some(stats_collector(&scratch.0.join("stats.jsonl"))),
    );
    let session: JsString = SESSION.into();
    let turn_id: JsString = "t1".into();
    recorder.start_turn(&session, &turn_id).unwrap();
    let model_port::ComposedPorts { execution, .. } = model_port::ports(
        router.clone(),
        obj(&[("providers", providers.clone())]),
        Some(DurableRecorder {
            recorder: recorder.clone(),
            recording_session_id: None,
        }),
        RouterModelPortOptions::main_agent(),
        &session,
    );
    let prepared = execution
        .prepare(PrepareInput {
            request: base_request(),
            context: session_context("t1"),
        })
        .await
        .expect("prepare");
    let abort = AbortFlag::new();
    let mut stream = execution
        .stream(StreamInput {
            prepared,
            context: session_context("t1"),
            abort: Some(abort.clone()),
        })
        .await
        .expect("stream");
    let first = stream.next().await.expect("pull").expect("event");
    assert_eq!(text(get(&first, "type")), "message_start");
    // The cancelled turn publishes nothing on its own.
    abort.abort();
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    assert_no_execute_failed(&trace);
    assert_eq!(stats_scope(&router), "project");
    // The pull that consumes the second queued event still does not publish
    // (the generator resumes on the next pull past that yield).
    let second = stream.next().await.expect("pull").expect("event");
    assert_eq!(text(get(&second, "type")), "error");
    assert_no_execute_failed(&trace);
    // The following pull resumes the generator: execute_failed and
    // observeRequest run before the re-yielded buffered event arrives.
    let third = stream.next().await.expect("pull").expect("event");
    assert_eq!(text(get(&third, "type")), "message_start");
    let failures = trace
        .lock()
        .unwrap()
        .iter()
        .filter(|entry| json(entry).contains("pilotdeck_router_execute_failed"))
        .count();
    assert_eq!(failures, 1, "execute_failed published exactly once");
    let fourth = stream.next().await.expect("pull").expect("event");
    assert_eq!(text(get(&fourth, "type")), "error");
    assert!(stream.next().await.expect("pull").is_none(), "done");
    assert_eq!(
        stats_scope(&router),
        "session",
        "observeRequest ran behind the pulls"
    );
    let snapshot = router.usage_snapshot(Some(&session));
    let requests = items(get(&snapshot, "requests"));
    assert_eq!(requests.len(), 1, "one request stat record");
    assert_eq!(
        transcript_types(&transcript_path),
        vec![
            "turn_started",
            "step_started",
            "model_request",
            "model_stream_event",
            "model_stream_event",
            "model_stream_event",
            "model_stream_event",
        ],
        "every pulled event is on disk in pull order"
    );
}
