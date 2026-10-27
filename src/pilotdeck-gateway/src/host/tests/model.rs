//! Model module replay tests against the real TS oracle.
#![allow(clippy::result_large_err)]
use super::*;
use crate::host::{
    execution::module_response,
    modules::model::{
        Handler as ModelHandler, MetadataField, ModelExecution, ModelFuture, ModelMetadata,
        ModelStream, PrepareInput, PreparedInvocation, StickyRoute, StreamFuture, StreamInput,
        TurnContext, serialize_prepared, with_sticky,
    },
};
use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
};

fn oracle() -> JsValue {
    let value = crate::test_baseline::oracle(include_str!(
        "../../../tests/fixtures/host-model/oracle.json"
    ));
    assert_eq!(
        text(&value, "commit"),
        "cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85"
    );
    let sources = value.get("sources").unwrap().as_object().unwrap();
    assert_eq!(sources.len(), 650, "#303 exact import closure source count");
    assert!(sources.entries().iter().all(|(_, digest)| {
        digest
            .as_string()
            .is_some_and(|value| value.len_utf16() == 64)
    }));
    value
}
fn array<'a>(value: &'a JsValue, key: &str) -> &'a [JsValue] {
    let JsValue::Array(items) = value.get(key).unwrap() else {
        panic!("{key}")
    };
    items
}
fn text(value: &JsValue, key: &str) -> String {
    value
        .get(key)
        .and_then(JsValue::as_string)
        .unwrap()
        .to_utf8()
        .unwrap()
}
fn optional_text(value: &JsValue, key: &str) -> Option<JsString> {
    value.get(key).and_then(JsValue::as_string).cloned()
}
fn obj(entries: Vec<(&str, JsValue)>) -> JsValue {
    let mut object = JsObject::new();
    for (key, value) in entries {
        object.insert(key.into(), value);
    }
    JsValue::Object(object)
}
fn string(value: &str) -> JsValue {
    JsValue::String(value.into())
}

#[derive(Default)]
struct FakeState {
    prepares: VecDeque<JsValue>,
    streams: VecDeque<JsValue>,
    metadata: Option<JsValue>,
    log: Vec<JsValue>,
}
struct FakeExecution {
    state: Arc<Mutex<FakeState>>,
}
struct FakeMetadata {
    state: Arc<Mutex<FakeState>>,
}
struct FakeStream {
    state: Arc<Mutex<FakeState>>,
    steps: VecDeque<JsValue>,
    return_error: Option<JsValue>,
}

fn module_error(spec: &JsValue) -> ModuleError {
    let message = spec.get("message").and_then(JsValue::as_string).unwrap();
    let fields = spec
        .get("fields")
        .cloned()
        .unwrap_or(JsValue::Object(JsObject::new()));
    ModuleError::from_js_fields(message.clone(), &fields)
}
fn decode_metadata(value: &JsValue) -> Result<Option<JsValue>, ModuleError> {
    if let Some(object) = value.as_object() {
        if let Some(message) = object.get("throw").and_then(JsValue::as_string) {
            return Err(ModuleError::new(message.clone()));
        }
        if object.get("undefined").and_then(JsValue::as_bool) == Some(true) {
            return Ok(None);
        }
    }
    Ok(Some(decode(value)))
}
fn decode(value: &JsValue) -> JsValue {
    match value {
        JsValue::Object(object) => {
            if object.get("undefined").and_then(JsValue::as_bool) == Some(true) {
                return JsValue::Undefined;
            }
            if let Some(number) = object.get("nonfinite").and_then(JsValue::as_string) {
                return JsValue::Number(match number.to_utf8().unwrap().as_str() {
                    "NaN" => f64::NAN,
                    "Infinity" => f64::INFINITY,
                    "-Infinity" => f64::NEG_INFINITY,
                    other => panic!("unexpected nonfinite {other}"),
                });
            }
            let mut decoded = JsObject::new();
            for (key, value) in object.entries() {
                decoded.insert(key.clone(), decode(value));
            }
            JsValue::Object(decoded)
        }
        JsValue::Array(items) => JsValue::Array(items.iter().map(decode).collect()),
        other => other.clone(),
    }
}
impl FakeMetadata {
    fn field(&self, name: &str, args: Vec<JsValue>) -> Result<Option<JsValue>, ModuleError> {
        let (value, op) = {
            let mut state = self.state.lock().unwrap();
            state.log.push(obj(vec![
                ("op", string(&format!("metadata.{name}"))),
                ("args", JsValue::Array(args)),
            ]));
            (state.metadata.as_ref().unwrap().get(name).cloned(), name)
        };
        let _ = op;
        match value {
            Some(value) => decode_metadata(&value),
            None => Ok(None),
        }
    }
}
impl ModelMetadata for FakeMetadata {
    fn provides(&self, field: MetadataField) -> bool {
        let key = match field {
            MetadataField::MaxContextTokens => "maxContextTokens",
            MetadataField::MaxOutputTokens => "maxOutputTokens",
            MetadataField::TokenLimits => "tokenLimits",
            MetadataField::Protocol => "protocol",
            MetadataField::SupportsPromptCache => "supportsPromptCache",
        };
        self.state
            .lock()
            .unwrap()
            .metadata
            .as_ref()
            .and_then(JsValue::as_object)
            .is_some_and(|metadata| metadata.contains_key(key))
    }
    // The dispatcher hands the raw prepared route to every lookup
    // (`sidecarDefaultModuleDispatcher.ts:88,445-450`), so the logged args are
    // the raw route values, exactly like the TS fixture's fake.
    fn get_model_max_context_tokens(
        &self,
        provider: &JsValue,
        model: &JsValue,
    ) -> Result<Option<JsValue>, ModuleError> {
        self.field("maxContextTokens", vec![provider.clone(), model.clone()])
    }
    fn get_model_max_output_tokens(
        &self,
        provider: &JsValue,
        model: &JsValue,
    ) -> Result<Option<JsValue>, ModuleError> {
        self.field("maxOutputTokens", vec![provider.clone(), model.clone()])
    }
    fn get_model_token_limits(
        &self,
        provider: &JsValue,
        model: &JsValue,
    ) -> Result<Option<JsValue>, ModuleError> {
        self.field("tokenLimits", vec![provider.clone(), model.clone()])
    }
    fn get_model_protocol(&self, provider: &JsValue) -> Result<Option<JsValue>, ModuleError> {
        self.field("protocol", vec![provider.clone()])
    }
    fn get_model_supports_prompt_cache(
        &self,
        provider: &JsValue,
        model: &JsValue,
    ) -> Result<Option<JsValue>, ModuleError> {
        self.field("supportsPromptCache", vec![provider.clone(), model.clone()])
    }
}
impl ModelExecution for FakeExecution {
    fn prepare(&self, input: PrepareInput) -> ModelFuture<PreparedInvocation> {
        let state = self.state.clone();
        Box::pin(async move {
            let step = {
                let mut state = state.lock().unwrap();
                state.log.push(obj(vec![
                    ("op", string("prepare")),
                    ("request", input.request.clone()),
                    ("context", JsValue::Object(input.context.clone())),
                ]));
                state
                    .prepares
                    .pop_front()
                    .expect("prepare script exhausted")
            };
            if let Some(error) = step.get("error") {
                return Err(module_error(error));
            }
            let ok = step.get("ok").expect("prepare step");
            let mut prepared = PreparedInvocation::new(
                ok.get("request").unwrap().clone(),
                ok.get("provider").cloned().unwrap_or(JsValue::Undefined),
                ok.get("model").cloned().unwrap_or(JsValue::Undefined),
            );
            prepared.max_context_tokens = ok.get("maxContextTokens").cloned();
            prepared.max_output_tokens = ok.get("maxOutputTokens").cloned();
            Ok(prepared)
        })
    }
    fn stream(&self, input: StreamInput) -> ModelFuture<Box<dyn ModelStream>> {
        let state = self.state.clone();
        Box::pin(async move {
            let step = {
                let mut state = state.lock().unwrap();
                state.log.push(obj(vec![
                    ("op", string("stream")),
                    ("prepared", serialize_prepared(&input.prepared)),
                    ("context", JsValue::Object(input.context.clone())),
                ]));
                state.streams.pop_front().expect("stream script exhausted")
            };
            if let Some(error) = step.get("createError") {
                return Err(module_error(error));
            }
            let steps = step
                .get("steps")
                .and_then(|value| match value {
                    JsValue::Array(items) => Some(items.iter().cloned().collect()),
                    _ => None,
                })
                .unwrap_or_default();
            Ok(Box::new(FakeStream {
                state,
                steps,
                return_error: step.get("returnError").cloned(),
            }) as Box<dyn ModelStream>)
        })
    }
}
impl ModelStream for FakeStream {
    fn next(&mut self) -> StreamFuture<'_, Option<JsValue>> {
        Box::pin(async move {
            self.state
                .lock()
                .unwrap()
                .log
                .push(obj(vec![("op", string("stream.next"))]));
            match self.steps.pop_front() {
                None => Ok(None),
                Some(step) => match step.get("error") {
                    Some(error) => Err(module_error(error)),
                    None => Ok(Some(
                        step.get("event").cloned().unwrap_or(JsValue::Undefined),
                    )),
                },
            }
        })
    }
    fn close(&mut self) -> StreamFuture<'_, ()> {
        Box::pin(async move {
            self.state
                .lock()
                .unwrap()
                .log
                .push(obj(vec![("op", string("stream.return"))]));
            match &self.return_error {
                Some(error) => Err(module_error(error)),
                None => Ok(()),
            }
        })
    }
}

fn turn_context(turn: &JsValue) -> TurnContext {
    TurnContext {
        session_id: turn
            .get("sessionId")
            .and_then(JsValue::as_string)
            .unwrap()
            .clone(),
        turn_id: turn
            .get("turnId")
            .and_then(JsValue::as_string)
            .unwrap()
            .clone(),
        operation_deadline: optional_text(turn, "operationDeadline"),
        workspace_id: optional_text(turn, "workspaceId"),
        storage_config_version: optional_text(turn, "storageConfigVersion"),
        abort: None,
    }
}
fn sticky_route(value: &JsValue) -> StickyRoute {
    StickyRoute {
        previous_tier: optional_text(value, "previousTier"),
        previous_provider: optional_text(value, "previousProvider"),
        previous_model: optional_text(value, "previousModel"),
        orchestrating: value
            .get("orchestrating")
            .and_then(JsValue::as_bool)
            .unwrap_or(false),
    }
}
fn log_slice(state: &Arc<Mutex<FakeState>>, start: usize) -> JsValue {
    JsValue::Array(state.lock().unwrap().log[start..].to_vec())
}

#[tokio::test]
async fn real_ts_model_module_lifecycle() {
    replay_scenarios(None).await;
}

#[tokio::test]
async fn non_object_materialize_fallback_matches_real_ts() {
    for name in ["string", "string_utf16", "array", "null", "number"] {
        replay_scenarios(Some(&format!("materialize_fallback_{name}"))).await;
    }
}

#[tokio::test]
async fn execution_context_preserves_js_values_like_real_ts() {
    replay_scenarios(Some("execution_context_values")).await;
}

async fn replay_scenarios(filter: Option<&str>) {
    let oracle = oracle();
    let scenarios = array(&oracle, "scenarios");
    assert_eq!(scenarios.len(), 43);
    let mut call_count = 0;
    for scenario in scenarios {
        let name = text(scenario, "name");
        if filter.is_some_and(|filter| name != filter) {
            continue;
        }
        let script = scenario.get("script").unwrap();
        let metadata_script = script.get("metadata").unwrap();
        let state = Arc::new(Mutex::new(FakeState {
            prepares: array(script, "prepares").iter().cloned().collect(),
            streams: array(script, "streams").iter().cloned().collect(),
            metadata: metadata_script.as_object().cloned().map(JsValue::Object),
            log: Vec::new(),
        }));
        let execution: Arc<dyn ModelExecution> = if let Some(routing) = script.get("routing") {
            let route = routing
                .get("sticky")
                .and_then(JsValue::as_object)
                .map(|value| sticky_route(&JsValue::Object(value.clone())));
            with_sticky(
                Arc::new(FakeExecution {
                    state: state.clone(),
                }),
                route,
            )
        } else {
            Arc::new(FakeExecution {
                state: state.clone(),
            })
        };
        let metadata: Option<Arc<dyn ModelMetadata>> = metadata_script
            .as_object()
            .filter(|value| !value.is_empty())
            .map(|_| {
                Arc::new(FakeMetadata {
                    state: state.clone(),
                }) as Arc<dyn ModelMetadata>
            });
        let handler = ModelHandler::with_ports(
            execution,
            metadata,
            turn_context(scenario.get("turn").unwrap()),
        );
        if let Some(routing) = scenario.get("routing") {
            let calls = array(scenario, "routingCalls");
            assert_eq!(calls.len(), 1, "{name}");
            assert_eq!(text(&calls[0], "op"), "invalidateSticky", "{name}");
            assert_eq!(
                text(&calls[0], "sessionId"),
                turn_context(scenario.get("turn").unwrap())
                    .session_id
                    .to_utf8()
                    .unwrap(),
                "{name}"
            );
            assert!(routing.get("sticky").is_some(), "{name}");
        }
        for case in array(scenario, "calls") {
            call_count += 1;
            let call = case.get("call").unwrap();
            let start = state.lock().unwrap().log.len();
            let result = handler.call(call.clone()).await;
            // The TS oracle records JSON wire responses: undefined object
            // members disappear here just as they do in the real transport.
            let response =
                if scenario.get("dispatcherOnly").and_then(JsValue::as_bool) == Some(true) {
                    JsValue::Object(result.unwrap())
                } else {
                    module_response(call, result)
                };
            let response = parse_json(
                &pilotdeck_protocol::stringify_json(&response)
                    .unwrap()
                    .unwrap(),
            )
            .unwrap();
            assert_eq!(
                &response,
                case.get("response").unwrap(),
                "{name}: call={call:?}"
            );
            assert_eq!(
                &log_slice(&state, start),
                case.get("portCalls").unwrap(),
                "{name}: port calls for {call:?}"
            );
        }
        let dispose = scenario.get("dispose").unwrap();
        let start = state.lock().unwrap().log.len();
        handler.dispose().await.unwrap();
        assert_eq!(
            &log_slice(&state, start),
            dispose.get("portCalls").unwrap(),
            "{name}: dispose"
        );
        {
            let state = state.lock().unwrap();
            assert!(state.prepares.is_empty(), "{name}: prepares remain");
            assert!(state.streams.is_empty(), "{name}: streams remain");
        }
        if name == "dispose_clears_streams" {
            let call = obj(vec![
                ("kind", string("request")),
                ("method", string("module_call")),
                ("messageId", string("after-dispose")),
                ("requestId", string("callback-request")),
                ("runId", string("run")),
                ("operationId", string("operation")),
                ("module", string("model")),
                (
                    "payload",
                    obj(vec![
                        ("operation", string("stream_next")),
                        ("preparationId", string("da")),
                        (
                            "request",
                            parse_json(r#"{"provider":"p","model":"m","messages":[]}"#).unwrap(),
                        ),
                    ]),
                ),
            ]);
            let response = module_response(&call, handler.call(call.clone()).await);
            assert_eq!(response.get("ok").and_then(JsValue::as_bool), Some(false));
            assert_eq!(
                response
                    .get("error")
                    .and_then(|error| error.get("message"))
                    .and_then(JsValue::as_string)
                    .unwrap()
                    .to_utf8()
                    .unwrap(),
                "Unknown sidecar model preparation: da"
            );
        }
    }
    if filter.is_none() {
        assert_eq!(call_count, 134);
    } else {
        assert!(call_count > 0, "scenario filter must match");
    }
}

#[tokio::test]
async fn raw_route_metadata_lookups_match_real_ts() {
    replay_scenarios(Some("prepare_metadata_raw_route_invalid")).await;
    replay_scenarios(Some("prepare_metadata_raw_route_valid")).await;
}

#[tokio::test]
async fn sticky_create_error_removes_preparation_like_ts() {
    replay_scenarios(Some("sticky_create_error_cleans_preparation")).await;
}

#[tokio::test]
async fn undefined_optional_token_limits_match_ts() {
    replay_scenarios(Some("metadata_limits_undefined_output")).await;
    replay_scenarios(Some("metadata_limits_undefined_context")).await;
}

#[tokio::test]
async fn empty_sticky_strings_preserve_remote_metadata_like_ts() {
    replay_scenarios(Some("sticky_empty_strings_preserve_metadata")).await;
}

/// Gates use explicit polling, not sleeps, to place cancellation at the exact
/// await being tested. No background task survives a test or failed assertion.
struct Gate(tokio::sync::watch::Sender<bool>);
impl Default for Gate {
    fn default() -> Self {
        Self(tokio::sync::watch::channel(false).0)
    }
}
impl Gate {
    fn release(&self) {
        self.0.send_replace(true);
    }
    async fn wait(&self) {
        self.0.subscribe().wait_for(|open| *open).await.unwrap();
    }
}

#[derive(Clone, Default)]
struct ControlledExecution {
    log: Arc<Mutex<Vec<String>>>,
    create_gate: Option<Arc<Gate>>,
    next_gate: Option<Arc<Gate>>,
    close_gate: Option<Arc<Gate>>,
    gated_id: Option<String>,
    close_error_id: Option<String>,
    yields_event: bool,
}
impl ControlledExecution {
    fn record(&self, event: &str, id: &str) {
        self.log.lock().unwrap().push(format!("{event}:{id}"));
    }
    fn entries(&self, event: &str) -> Vec<String> {
        self.log
            .lock()
            .unwrap()
            .iter()
            .filter_map(|entry| entry.strip_prefix(&format!("{event}:")))
            .map(str::to_owned)
            .collect()
    }
    fn handler(&self) -> ModelHandler {
        ModelHandler::with_ports(Arc::new(self.clone()), None, TurnContext::default())
    }
    fn is_gated(&self, id: &str) -> bool {
        self.gated_id.as_deref().is_none_or(|gated| gated == id)
    }
    fn entry_values(&self, event: &str) -> JsValue {
        JsValue::Array(self.entries(event).iter().map(|id| string(id)).collect())
    }
}
impl ModelExecution for ControlledExecution {
    fn prepare(&self, input: PrepareInput) -> ModelFuture<PreparedInvocation> {
        Box::pin(async move {
            Ok(PreparedInvocation::new(
                input.request,
                "p".into(),
                "m".into(),
            ))
        })
    }
    fn stream(&self, input: StreamInput) -> ModelFuture<Box<dyn ModelStream>> {
        let control = self.clone();
        Box::pin(async move {
            let id = text(&input.prepared.request, "id");
            control.record("create", &id);
            if let Some(gate) = &control.create_gate {
                gate.wait().await;
            }
            control.record("created", &id);
            Ok(Box::new(ControlledStream {
                control,
                id,
                step: 0,
            }) as Box<dyn ModelStream>)
        })
    }
}
struct ControlledStream {
    control: ControlledExecution,
    id: String,
    step: usize,
}
impl ModelStream for ControlledStream {
    fn next(&mut self) -> StreamFuture<'_, Option<JsValue>> {
        Box::pin(async move {
            self.control.record("next", &self.id);
            if let Some(gate) = &self.control.next_gate
                && self.control.is_gated(&self.id)
            {
                gate.wait().await;
            }
            self.step += 1;
            Ok(self.control.yields_event.then(|| {
                obj(vec![
                    ("type", string("text_delta")),
                    ("text", string(&format!("{}-{}", self.id, self.step))),
                    ("raw", obj(vec![("vendor", string("fixture"))])),
                ])
            }))
        })
    }
    fn close(&mut self) -> StreamFuture<'_, ()> {
        Box::pin(async move {
            self.control.record("close", &self.id);
            if let Some(gate) = &self.control.close_gate
                && self.control.is_gated(&self.id)
            {
                gate.wait().await;
            }
            if self.control.close_error_id.as_deref() == Some(&self.id) {
                return Err(ModuleError::new(format!("close failed: {}", self.id)));
            }
            self.control.record("closed", &self.id);
            Ok(())
        })
    }
}
fn model_call(operation: &str, id: &str) -> JsValue {
    obj(vec![(
        "payload",
        obj(vec![
            ("operation", string(operation)),
            ("preparationId", string(id)),
            (
                "request",
                obj(vec![
                    ("provider", string("p")),
                    ("model", string("m")),
                    ("messages", JsValue::Array(Vec::new())),
                    ("id", string(id)),
                ]),
            ),
        ]),
    )])
}
async fn pending<T>(future: &mut HostFuture<T>) {
    std::future::poll_fn(|cx| {
        assert!(
            future.as_mut().poll(cx).is_pending(),
            "expected controlled await"
        );
        std::task::Poll::Ready(())
    })
    .await;
}
async fn finish<T>(future: HostFuture<T>) -> Result<T, HostError> {
    tokio::time::timeout(Duration::from_secs(2), future)
        .await
        .expect("controlled operation timed out")
}
fn done() -> JsValue {
    parse_json(r#"{"events":[],"done":true}"#).unwrap()
}
fn concurrent_gold(key: &str) -> JsValue {
    oracle()
        .get("concurrency")
        .unwrap()
        .get(key)
        .unwrap()
        .clone()
}
async fn prepare_model(handler: &ModelHandler, id: &str) {
    finish(handler.call(model_call("prepare", id)))
        .await
        .unwrap();
}

#[tokio::test]
async fn dispose_starts_all_closes_before_waiting_like_ts() {
    let gate = Arc::new(Gate::default());
    let control = ControlledExecution {
        close_gate: Some(gate.clone()),
        yields_event: true,
        ..Default::default()
    };
    let handler = control.handler();
    for id in ["a", "b"] {
        prepare_model(&handler, id).await;
        finish(handler.call(model_call("stream_next", id)))
            .await
            .unwrap();
    }
    let mut disposal = handler.dispose();
    pending(&mut disposal).await;
    let started = JsValue::Array(
        control
            .entries("close")
            .iter()
            .map(|id| string(id))
            .collect(),
    );
    assert_eq!(&started, concurrent_gold("dispose").get("started").unwrap());
    gate.release();
    finish(disposal).await.unwrap();
    assert_eq!(control.entries("closed"), ["a", "b"]);
}

#[tokio::test]
async fn concurrent_first_next_shares_pending_creation() {
    let gate = Arc::new(Gate::default());
    let control = ControlledExecution {
        create_gate: Some(gate.clone()),
        ..Default::default()
    };
    let handler = control.handler();
    prepare_model(&handler, "a").await;
    let mut first = handler.call(model_call("stream_next", "a"));
    let mut second = handler.call(model_call("stream_next", "a"));
    pending(&mut first).await;
    pending(&mut second).await;
    assert_eq!(
        JsValue::Number(control.entries("create").len() as f64),
        *concurrent_gold("eof").get("createsBeforeRelease").unwrap()
    );
    gate.release();
    assert_eq!(JsValue::Object(finish(first).await.unwrap()), done());
    assert_eq!(JsValue::Object(finish(second).await.unwrap()), done());
}

async fn accepted_next_before_close(key: &str, count: usize, creating: bool, close_fails: bool) {
    let gate = Arc::new(Gate::default());
    let close_gate = Arc::new(Gate::default());
    let control = ControlledExecution {
        create_gate: creating.then(|| gate.clone()),
        next_gate: (!creating).then(|| gate.clone()),
        close_gate: Some(close_gate.clone()),
        close_error_id: close_fails.then(|| "a".to_owned()),
        yields_event: true,
        ..Default::default()
    };
    let handler = control.handler();
    prepare_model(&handler, "a").await;
    let mut calls = Vec::new();
    for _ in 0..count {
        let mut next = handler.call(model_call("stream_next", "a"));
        pending(&mut next).await;
        calls.push(next);
    }
    let mut close = handler.call(model_call("close_stream", "a"));
    pending(&mut close).await;
    let gold = concurrent_gold(key);
    let after = finish(handler.call(model_call("stream_next", "a")))
        .await
        .unwrap_err();
    assert_eq!(
        JsValue::String(after.module_error.unwrap().message),
        *gold.get("after").unwrap().get("error").unwrap()
    );
    gate.release();
    let mut responses = Vec::new();
    for next in calls {
        // Every accepted next finishes while return() is still gated.
        responses.push(JsValue::Object(finish(next).await.unwrap()));
    }
    assert_eq!(JsValue::Array(responses), *gold.get("responses").unwrap());
    pending(&mut close).await;
    assert_eq!(control.entries("close"), ["a"]);
    close_gate.release();
    let close_response = match finish(close).await {
        Ok(value) => obj(vec![("value", JsValue::Object(value))]),
        Err(error) => obj(vec![(
            "error",
            JsValue::String(error.module_error.unwrap().message),
        )]),
    };
    assert_eq!(close_response, *gold.get("close").unwrap());
    finish(handler.dispose()).await.unwrap();
    let trace = JsValue::Array(
        control
            .log
            .lock()
            .unwrap()
            .iter()
            .map(|entry| string(entry))
            .collect(),
    );
    assert_eq!(trace, *gold.get("trace").unwrap());
}

#[tokio::test]
async fn close_during_creation_preserves_accepted_next_like_ts() {
    accepted_next_before_close("creationBeforeClose", 1, true, false).await;
}

#[tokio::test]
async fn accepted_next_calls_precede_close_like_ts() {
    accepted_next_before_close("acceptedBeforeClose", 2, false, false).await;
}

#[tokio::test]
async fn accepted_next_calls_do_not_receive_close_error_like_ts() {
    accepted_next_before_close("acceptedBeforeCloseError", 2, true, true).await;
}

#[tokio::test]
async fn dispose_close_error_does_not_skip_other_streams_like_ts() {
    let control = ControlledExecution {
        close_error_id: Some("a".into()),
        yields_event: true,
        ..Default::default()
    };
    let handler = control.handler();
    for id in ["a", "b"] {
        prepare_model(&handler, id).await;
        finish(handler.call(model_call("stream_next", id)))
            .await
            .unwrap();
    }
    finish(handler.dispose()).await.unwrap();
    let gold = concurrent_gold("disposeFailure");
    assert_eq!(control.entry_values("close"), *gold.get("started").unwrap());
    assert_eq!(
        control.entry_values("closed"),
        *gold.get("completed").unwrap()
    );
}

#[tokio::test]
async fn pending_next_and_close_do_not_block_other_streams_like_ts() {
    let next_gate = Arc::new(Gate::default());
    let close_gate = Arc::new(Gate::default());
    let control = ControlledExecution {
        next_gate: Some(next_gate.clone()),
        close_gate: Some(close_gate.clone()),
        gated_id: Some("a".into()),
        yields_event: true,
        ..Default::default()
    };
    let handler = control.handler();
    let gold = concurrent_gold("crossStreams");
    prepare_model(&handler, "a").await;
    let mut next = handler.call(model_call("stream_next", "a"));
    pending(&mut next).await;
    prepare_model(&handler, "b").await;
    assert_eq!(
        JsValue::Object(
            finish(handler.call(model_call("stream_next", "b")))
                .await
                .unwrap()
        ),
        *gold.get("bNext").unwrap()
    );
    next_gate.release();
    assert_eq!(
        JsValue::Object(finish(next).await.unwrap()),
        *gold.get("aNext").unwrap()
    );
    let mut close = handler.call(model_call("close_stream", "a"));
    pending(&mut close).await;
    assert_eq!(
        JsValue::Object(
            finish(handler.call(model_call("close_stream", "b")))
                .await
                .unwrap()
        ),
        *gold.get("bClose").unwrap()
    );
    assert_eq!(
        control.entry_values("close"),
        *gold.get("startedBeforeRelease").unwrap()
    );
    assert_eq!(
        control.entry_values("closed"),
        *gold.get("completedBeforeRelease").unwrap()
    );
    close_gate.release();
    assert_eq!(
        JsValue::Object(finish(close).await.unwrap()),
        *gold.get("aClose").unwrap()
    );
    finish(handler.dispose()).await.unwrap();
    assert_eq!(
        control.entry_values("closed"),
        *gold.get("completed").unwrap()
    );
}

#[tokio::test]
async fn accepted_concurrent_next_calls_both_observe_eof_like_ts() {
    let gate = Arc::new(Gate::default());
    let control = ControlledExecution {
        next_gate: Some(gate.clone()),
        ..Default::default()
    };
    let handler = control.handler();
    prepare_model(&handler, "a").await;
    let mut first = handler.call(model_call("stream_next", "a"));
    let mut second = handler.call(model_call("stream_next", "a"));
    pending(&mut first).await;
    pending(&mut second).await;
    gate.release();
    let responses = JsValue::Array(vec![
        JsValue::Object(finish(first).await.unwrap()),
        JsValue::Object(finish(second).await.unwrap()),
    ]);
    let gold = concurrent_gold("eof");
    assert_eq!(&responses, gold.get("responses").unwrap());
    let error = finish(handler.call(model_call("stream_next", "a")))
        .await
        .unwrap_err();
    assert_eq!(
        JsValue::String(error.module_error.unwrap().message),
        *gold.get("after").unwrap().get("error").unwrap()
    );
}

#[tokio::test]
async fn cancelled_close_waiting_for_next_remains_owned_by_dispose() {
    let gate = Arc::new(Gate::default());
    let control = ControlledExecution {
        next_gate: Some(gate),
        ..Default::default()
    };
    let handler = control.handler();
    prepare_model(&handler, "a").await;
    let mut next = handler.call(model_call("stream_next", "a"));
    pending(&mut next).await;
    let mut close = handler.call(model_call("close_stream", "a"));
    pending(&mut close).await;
    drop(close);
    drop(next); // TurnScope cancels and joins callbacks before disposal.
    finish(handler.dispose()).await.unwrap();
    assert_eq!(
        JsValue::Number(control.entries("close").len() as f64),
        *concurrent_gold("close").get("returnsAfterDispose").unwrap()
    );
    assert_eq!(control.entries("closed"), ["a"]);
}

#[tokio::test]
async fn cancelled_started_close_is_resumed_without_repeating_return() {
    let gate = Arc::new(Gate::default());
    let control = ControlledExecution {
        close_gate: Some(gate.clone()),
        yields_event: true,
        ..Default::default()
    };
    let handler = control.handler();
    prepare_model(&handler, "a").await;
    finish(handler.call(model_call("stream_next", "a")))
        .await
        .unwrap();
    let mut close = handler.call(model_call("close_stream", "a"));
    pending(&mut close).await;
    assert_eq!(control.entries("close"), ["a"]);
    drop(close);
    let mut disposal = handler.dispose();
    pending(&mut disposal).await;
    assert_eq!(control.entries("close"), ["a"]);
    gate.release();
    finish(disposal).await.unwrap();
    assert_eq!(control.entries("closed"), ["a"]);
}

#[tokio::test]
async fn cancelled_creation_is_resumed_and_closed_by_dispose() {
    let gate = Arc::new(Gate::default());
    let control = ControlledExecution {
        create_gate: Some(gate.clone()),
        ..Default::default()
    };
    let handler = control.handler();
    prepare_model(&handler, "a").await;
    let mut next = handler.call(model_call("stream_next", "a"));
    pending(&mut next).await;
    drop(next);
    let mut disposal = handler.dispose();
    pending(&mut disposal).await;
    gate.release();
    finish(disposal).await.unwrap();
    assert_eq!(control.entries("create"), ["a"]);
    assert_eq!(control.entries("close"), ["a"]);
    assert!(control.entries("next").is_empty());
}

#[tokio::test]
async fn unstarted_sticky_close_and_next_do_not_consume_tier_like_ts() {
    let request = model_call("prepare", "a")
        .get("payload")
        .unwrap()
        .get("request")
        .unwrap()
        .clone();
    let prepared = PreparedInvocation::new(request.clone(), "p".into(), "m".into());
    let state = Arc::new(Mutex::new(FakeState {
        prepares: VecDeque::from([obj(vec![("ok", serialize_prepared(&prepared))])]),
        ..Default::default()
    }));
    let execution = with_sticky(
        Arc::new(FakeExecution {
            state: state.clone(),
        }),
        Some(StickyRoute {
            previous_tier: Some("sticky-tier".into()),
            ..Default::default()
        }),
    );
    let mut stream = execution
        .stream(StreamInput {
            prepared,
            context: JsObject::new(),
            abort: None,
        })
        .await
        .unwrap();
    stream.close().await.unwrap();
    assert!(stream.next().await.unwrap().is_none());
    drop(stream);
    execution
        .prepare(PrepareInput {
            request,
            context: JsObject::new(),
        })
        .await
        .unwrap();
    let log = &state.lock().unwrap().log;
    let gold = concurrent_gold("stickyUnstarted");
    let creates = log
        .iter()
        .filter(|entry| text(entry, "op") == "stream")
        .count();
    assert_eq!(
        JsValue::Number(creates as f64),
        *gold.get("creates").unwrap()
    );
    assert_eq!(
        log.last().unwrap().get("context").unwrap().get("metadata"),
        gold.get("metadata")
    );
}

#[tokio::test]
async fn close_accepted_before_eof_still_returns_the_iterator() {
    let gate = Arc::new(Gate::default());
    let control = ControlledExecution {
        next_gate: Some(gate.clone()),
        ..Default::default()
    };
    let handler = control.handler();
    prepare_model(&handler, "a").await;
    let mut next = handler.call(model_call("stream_next", "a"));
    pending(&mut next).await;
    let mut close = handler.call(model_call("close_stream", "a"));
    pending(&mut close).await;
    gate.release();
    assert_eq!(JsValue::Object(finish(next).await.unwrap()), done());
    assert_eq!(
        JsValue::Object(finish(close).await.unwrap()),
        *concurrent_gold("close").get("response").unwrap()
    );
    assert_eq!(control.entries("close"), ["a"]);
    finish(handler.dispose()).await.unwrap();
    assert_eq!(control.entries("close"), ["a"]);
}

/// Fix round 1: `DurableModelStream`'s transcript appends are synchronous
/// file I/O (`open(append)` + `write_all` per record); they must run on
/// `cpu::run`'s bound blocking pool, not the executor thread. Hold all four
/// permits, then prove the first pull — which records the request before
/// yielding anything — waits for one, and completes with the on-disk order
/// intact once a permit frees. A regression to the inline call completes
/// within the timeout and fails the wait assertion.
#[tokio::test(flavor = "current_thread")]
async fn durable_stream_recording_shares_the_cpu_permit_pool() {
    use crate::host::modules::model_port::{self, DurableRecorder, RouterModelPortOptions};
    use crate::runtime::model::cpu;
    use crate::runtime::model::fallback::RouterStreamFuture;
    use crate::runtime::model::http::{StreamOptions, StreamOutput};
    use crate::runtime::model::router::{
        RouterConfig, RouterDeps, RouterModelInvocation, RouterRuntime,
    };
    use crate::storage::recorder::Recorder;
    use crate::storage::{JsonlTranscriptStore, TranscriptKey};

    struct OneEvent;
    impl RouterModelInvocation for OneEvent {
        fn get_capabilities(&self, _: &JsString, _: &JsString) -> Result<JsValue, ModuleError> {
            Ok(obj(vec![]))
        }
        fn get_multimodal(&self, _: &JsString, _: &JsString) -> Result<JsValue, ModuleError> {
            Ok(obj(vec![("input", JsValue::Array(vec![]))]))
        }
        fn get_provider_protocol(&self, _: &JsString) -> JsString {
            "anthropic".into()
        }
        fn stream<'a>(
            &'a self,
            _: &'a JsValue,
            options: &'a StreamOptions,
            sink: &'a mut (dyn FnMut(StreamOutput) + Send),
        ) -> RouterStreamFuture<'a> {
            Box::pin(async move {
                sink(StreamOutput::Event(obj(vec![(
                    "type",
                    string("message_start"),
                )])));
                options.yield_boundary().await;
                Ok(())
            })
        }
    }

    let mut deps = RouterDeps::new(Arc::new(OneEvent));
    deps.clock = Arc::new(|| 0.0);
    let snapshot = obj(vec![(
        "config",
        obj(vec![(
            "router",
            obj(vec![
                ("enabled", JsValue::Bool(true)),
                (
                    "scenarios",
                    obj(vec![(
                        "default",
                        parse_json(r#"{"id":"a/m","provider":"a","model":"m"}"#).unwrap(),
                    )]),
                ),
            ]),
        )]),
    )]);
    let router = Arc::new(
        RouterRuntime::with_deps(RouterConfig::from_snapshot(&snapshot), deps)
            .expect("router runtime"),
    );
    let scratch = std::env::temp_dir().join(format!(
        "pdrs-a69-cpu-permit-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    std::fs::create_dir_all(&scratch).unwrap();
    let transcript_path = scratch.join("cpu-permit-s1.jsonl");
    let ids = Arc::new(Mutex::new(0));
    let next_id = ids.clone();
    let recorder = Recorder::with_sources(
        Arc::new(JsonlTranscriptStore::new()),
        TranscriptKey {
            project_root: scratch.clone(),
            pilot_home: scratch.join("home"),
            session_id: "s1".into(),
            transcript_path: transcript_path.clone(),
        },
        Arc::new(|| Ok("2026-10-18T00:00:00.000Z".to_string())),
        Arc::new(move || {
            let mut ids = next_id.lock().unwrap();
            *ids += 1;
            Ok(format!("id-{ids}"))
        }),
    );
    let session: JsString = "s1".into();
    recorder.start_turn(&session, &"t1".into()).unwrap();
    let model_port::ComposedPorts { execution, .. } = model_port::ports(
        router,
        obj(vec![("providers", obj(vec![]))]),
        Some(DurableRecorder {
            recorder: Arc::new(recorder),
            recording_session_id: None,
        }),
        RouterModelPortOptions::main_agent(),
        &session,
    );
    let mut context = JsObject::new();
    context.insert("sessionId".into(), string("s1"));
    context.insert("turnId".into(), string("t1"));
    context.insert("runId".into(), string("t1"));
    let stream_context = context.clone();
    let prepared = execution
        .prepare(PrepareInput {
            request: obj(vec![
                ("provider", string("a")),
                ("model", string("m")),
                (
                    "messages",
                    parse_json(r#"[{"role":"user","content":[{"type":"text","text":"hello"}]}]"#)
                        .unwrap(),
                ),
                ("maxOutputTokens", JsValue::Number(100.0)),
            ]),
            context,
        })
        .await
        .expect("prepare");
    let mut stream = execution
        .stream(StreamInput {
            prepared,
            context: stream_context,
            abort: None,
        })
        .await
        .expect("stream");

    // Hold all four cpu permits, like the router cache-policy regression.
    let started = Arc::new(AtomicUsize::new(0));
    let mut releases = Vec::new();
    let mut holders = Vec::new();
    for _ in 0..4 {
        let (release, wait) = std::sync::mpsc::channel::<()>();
        releases.push(release);
        let started = started.clone();
        holders.push(tokio::spawn(async move {
            cpu::run(move || {
                started.fetch_add(1, Ordering::SeqCst);
                let _ = wait.recv();
                Ok::<_, crate::host::HostError>(())
            })
            .await
        }));
    }
    let deadline = Instant::now() + std::time::Duration::from_secs(5);
    while started.load(Ordering::SeqCst) < 4 {
        assert!(
            Instant::now() < deadline,
            "four cpu permits were not acquired"
        );
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }

    let mut next = tokio::spawn(async move { stream.next().await });
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(500), &mut next)
            .await
            .is_err(),
        "the recording first pull must wait for a cpu permit while all four are held"
    );

    for release in releases {
        release.send(()).expect("release cpu holder");
    }
    for holder in holders {
        holder.await.expect("cpu holder task").expect("cpu holder");
    }
    let event = tokio::time::timeout(std::time::Duration::from_secs(5), next)
        .await
        .expect("the first pull resumes after a permit frees")
        .expect("first pull task")
        .expect("first pull");
    let event = event.expect("one scripted event");
    assert_eq!(text(&event, "type"), "message_start");
    let transcript = std::fs::read_to_string(&transcript_path).unwrap();
    let types: Vec<String> = transcript
        .lines()
        .map(|line| text(&parse_json(line).unwrap(), "type"))
        .collect();
    // The record-before-yield order survives the off-thread hop.
    assert_eq!(
        types,
        vec![
            "turn_started",
            "step_started",
            "model_request",
            "model_stream_event",
        ]
    );
    std::fs::remove_dir_all(&scratch).unwrap();
}
