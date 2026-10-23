//! Real TS lifecycle oracle through the native factory, router, durable model
//! port and provider HTTP layer. The peer is an owned streaming HTTP body;
//! closing it before EOF proves cancellation reached the HTTP boundary.
#[path = "common/baseline.rs"]
mod baseline;
use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::{
    Request, Response,
    body::{Body, Frame},
};
use pilotdeck_gateway::{
    config::Env,
    host::{
        ModuleError,
        modules::{
            model::{ModelStream, PrepareInput, StreamInput, TurnContext},
            model_port::{ComposedPorts, DurableRecorder, RouterModelPortOptions},
        },
    },
    runtime::{
        model::{
            fallback::RouterStreamFuture,
            http::{
                BodyFailure, ModelHttpTransport, StreamOptions, StreamOutput, TransportFuture,
                YieldGate,
            },
            provider_registry::{
                ModelInvocationProvider, ModelInvocationProviderRegistry,
                NativeModelInvocationProvider,
            },
            router::RouterModelInvocation,
        },
        network::ExchangeFuture,
        project::{
            model::{ModelFamilySource, ModelResources, RouterResources},
            *,
        },
    },
    storage::{
        JsonlTranscriptStore, StoreResult, TranscriptKey, TranscriptRead, TranscriptStore,
        recorder::Recorder,
    },
};
use pilotdeck_protocol::{JsObject, JsString, JsValue, parse_json, stringify_json};
use std::{
    any::Any,
    collections::HashMap,
    future::Future,
    path::{Path, PathBuf},
    pin::Pin,
    sync::{Arc, Mutex},
    task::{Context, Poll},
    time::{Duration, SystemTime},
};
use tokio::sync::mpsc;
const ORACLE: &str = include_str!("fixtures/model-factory/oracle.json");
type Trace = Arc<Mutex<Vec<String>>>;
fn push(trace: &Trace, value: impl Into<String>) {
    trace.lock().unwrap().push(value.into());
}
fn has(trace: &Trace, value: &str) -> bool {
    trace.lock().unwrap().iter().any(|item| item == value)
}
fn get<'a>(value: &'a JsValue, key: &str) -> &'a JsValue {
    static UNDEFINED: JsValue = JsValue::Undefined;
    value.get(key).unwrap_or(&UNDEFINED)
}
fn text(value: &JsValue) -> String {
    value.as_string().unwrap().to_utf8().unwrap()
}
fn strings(value: &JsValue) -> Vec<String> {
    if let JsValue::Array(values) = value {
        values.iter().map(text).collect()
    } else {
        panic!("array")
    }
}
async fn bounded<T>(future: impl Future<Output = T>) -> T {
    tokio::time::timeout(Duration::from_secs(8), future)
        .await
        .expect("bounded operation hung")
}
async fn until(mut condition: impl FnMut() -> bool) {
    bounded(async {
        while !condition() {
            tokio::task::yield_now().await;
        }
    })
    .await
}
struct Scratch(PathBuf);
impl Scratch {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("pdrs-584-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).unwrap();
    }
}

struct PeerBody {
    receiver: mpsc::UnboundedReceiver<Bytes>,
    trace: Trace,
    label: String,
}
impl Body for PeerBody {
    type Data = Bytes;
    type Error = BodyFailure;
    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, BodyFailure>>> {
        self.receiver
            .poll_recv(cx)
            .map(|chunk| chunk.map(|bytes| Ok(Frame::data(bytes))))
    }
}
impl Drop for PeerBody {
    fn drop(&mut self) {
        if !self.receiver.is_closed() {
            push(&self.trace, format!("http-cancel:{}", self.label));
        }
    }
}
struct Peer {
    frames: JsValue,
    trace: Trace,
    pending: Mutex<HashMap<String, mpsc::UnboundedSender<Bytes>>>,
}
impl Peer {
    fn finish(&self, label: &str) {
        let sender = self.pending.lock().unwrap().remove(label).unwrap();
        let bytes = text(get(&self.frames, "text")) + &text(get(&self.frames, "end"));
        sender.send(Bytes::from(bytes)).unwrap();
    }
}
impl ModelHttpTransport for Peer {
    fn send(&self, request: Request<Full<Bytes>>) -> TransportFuture {
        let label = request.uri().path().split('/').nth(1).unwrap().to_string();
        push(&self.trace, format!("http:{label}"));
        let (sender, receiver) = mpsc::unbounded_channel();
        sender
            .send(Bytes::from(text(get(&self.frames, "start"))))
            .unwrap();
        self.pending.lock().unwrap().insert(label.clone(), sender);
        let body = PeerBody {
            receiver,
            label,
            trace: self.trace.clone(),
        }
        .boxed();
        let response = Response::builder()
            .status(200)
            .header("content-type", "text/event-stream")
            .body(body)
            .unwrap();
        ExchangeFuture::ready(Ok(response))
    }
}
struct OwnedProvider {
    inner: NativeModelInvocationProvider,
    trace: Trace,
    label: String,
}
impl ModelInvocationProvider for OwnedProvider {
    fn provider_id(&self) -> &JsString {
        self.inner.provider_id()
    }
    fn dispose(&self) -> DisposeFuture<'_> {
        Box::pin(async {
            push(&self.trace, format!("dispose:{}", self.label));
            Ok(())
        })
    }
}
impl RouterModelInvocation for OwnedProvider {
    fn get_capabilities(&self, p: &JsString, m: &JsString) -> Result<JsValue, ModuleError> {
        self.inner.get_capabilities(p, m)
    }
    fn get_multimodal(&self, p: &JsString, m: &JsString) -> Result<JsValue, ModuleError> {
        self.inner.get_multimodal(p, m)
    }
    fn get_provider_protocol(&self, p: &JsString) -> JsString {
        self.inner.get_provider_protocol(p)
    }
    fn stream<'a>(
        &'a self,
        request: &'a JsValue,
        options: &'a StreamOptions,
        sink: &'a mut (dyn FnMut(StreamOutput) + Send),
    ) -> RouterStreamFuture<'a> {
        self.inner.stream(request, options, sink)
    }
}
struct OtherFamily(Family);
impl FamilyResource for OtherFamily {
    fn dispose(&self) -> DisposeFuture<'_> {
        Box::pin(async { Ok(()) })
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}
impl ResourceFamilySource for OtherFamily {
    fn family(&self) -> Family {
        self.0
    }
    fn stage(&self, _: &StageContext, sink: &mut StageSink<'_>) -> RuntimeResult {
        sink.register_owner(self.0, Arc::new(Self(self.0)));
        Ok(())
    }
}
fn families() -> Families {
    let mut families = Families::unwired().with_native_models();
    families.plugins = Arc::new(OtherFamily(Family::PluginRuntime));
    families.router_custom_routers = Some(Arc::new(OtherFamily(Family::RouterCustomRouters)));
    families.lsp = Some(Arc::new(OtherFamily(Family::LspService)));
    families.execution_world = Arc::new(OtherFamily(Family::ExecutionWorld));
    families.context_storage = Arc::new(OtherFamily(Family::ContextStorage));
    families.session_title = Some(Arc::new(OtherFamily(Family::SessionTitleProvider)));
    families.mcp = Arc::new(OtherFamily(Family::McpProvider));
    families.memory = Arc::new(OtherFamily(Family::MemoryBundle));
    families
}
fn write_config(home: &Path, label: &str, cap: u64) {
    std::fs::write(home.join("pilotdeck.yaml"),format!("schemaVersion: 1\nagent: {{ model: a/m }}\nmodel:\n  providers:\n    a:\n      protocol: anthropic\n      url: http://fixture.test/{label}\n      apiKey: mock-only\n      retry: {{ streamMaxRetries: 0 }}\n      models:\n        m:\n          capabilities: {{ maxContextTokens: {cap}, maxOutputTokens: 64 }}\n")).unwrap();
}
fn registry(scratch: &Scratch, families: Families) -> ProjectRuntimeRegistry {
    let home = scratch.0.join("home");
    let project = scratch.0.join("project");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&project).unwrap();
    let mut env = Env::new();
    env.insert("PILOT_HOME".into(), home.to_str().unwrap().into());
    ProjectRuntimeRegistry::new(
        &project,
        Arc::new(ProjectResourcesFactory::new(ProjectResourcesOptions {
            pilot_home: home,
            cwd: scratch.0.clone(),
            env,
            homedir: Some(scratch.0.clone()),
            builtin_skills_root: None,
            now: Arc::new(|| SystemTime::UNIX_EPOCH),
            config_version: None,
            runtime_profile_overrides: Default::default(),
            families,
        })),
    )
}
fn context(runtime: &ProjectRuntime) -> StageContext {
    runtime
        .resources()
        .get::<ProjectResources>()
        .unwrap()
        .context()
}
fn model(runtime: &ProjectRuntime) -> Arc<dyn FamilyResource> {
    context(runtime).resource(Resource::Model).unwrap()
}
fn router(runtime: &ProjectRuntime) -> Arc<dyn FamilyResource> {
    context(runtime).resource(Resource::Router).unwrap()
}
struct RecordingStore {
    inner: JsonlTranscriptStore,
    trace: Trace,
    label: String,
}
impl TranscriptStore for RecordingStore {
    fn append(&self, key: &TranscriptKey, entry: &JsValue) -> StoreResult<()> {
        self.inner.append(key, entry)?;
        match text(get(entry, "type")).as_str() {
            "model_request" => push(
                &self.trace,
                format!(
                    "record-request:{}:{}",
                    self.label,
                    text(
                        get(get(entry, "request"), "metadata")
                            .get("marker")
                            .unwrap()
                    )
                ),
            ),
            "model_stream_event" => push(
                &self.trace,
                format!(
                    "record-event:{}:{}",
                    self.label,
                    text(get(get(entry, "event"), "type"))
                ),
            ),
            _ => {}
        }
        Ok(())
    }
    fn read(&self, key: &TranscriptKey, max: Option<u64>) -> StoreResult<TranscriptRead> {
        self.inner.read(key, max)
    }
}
fn ports(runtime: &ProjectRuntime, label: &str, trace: &Trace, scratch: &Scratch) -> ComposedPorts {
    let recorder = Arc::new(Recorder::new(
        Arc::new(RecordingStore {
            inner: JsonlTranscriptStore::new(),
            trace: trace.clone(),
            label: label.into(),
        }),
        TranscriptKey {
            project_root: scratch.0.join("project"),
            pilot_home: scratch.0.join("home"),
            session_id: label.into(),
            transcript_path: scratch.0.join(format!("{label}.jsonl")),
        },
    ));
    recorder.start_turn(&label.into(), &"t".into()).unwrap();
    router(runtime)
        .as_any()
        .downcast_ref::<RouterResources>()
        .unwrap()
        .ports(
            Some(DurableRecorder {
                recorder,
                recording_session_id: None,
            }),
            RouterModelPortOptions::default(),
            &TurnContext {
                session_id: label.into(),
                turn_id: "t".into(),
                operation_deadline: None,
                workspace_id: None,
                storage_config_version: None,
                abort: None,
            },
        )
}
fn turn_context(label: &str) -> JsObject {
    parse_json(&format!(r#"{{"sessionId":"{label}","turnId":"t"}}"#))
        .unwrap()
        .as_object()
        .unwrap()
        .clone()
}
async fn take(stream: &mut dyn ModelStream, label: &str, trace: &Trace) -> bool {
    match bounded(stream.next()).await.unwrap() {
        Some(event) => {
            push(
                trace,
                format!("yield:{label}:{}", text(get(&event, "type"))),
            );
            true
        }
        None => false,
    }
}

#[tokio::test]
async fn factory_reload_inflight_cancel_and_drain_match_ts() {
    let oracle = baseline::oracle(ORACLE);
    let scratch = Scratch::new();
    let trace = Trace::default();
    let peer = Arc::new(Peer {
        frames: get(&oracle, "frames").clone(),
        trace: trace.clone(),
        pending: Mutex::default(),
    });
    let mut families = families();
    let provider_trace = trace.clone();
    let transport = peer.clone();
    families.model_runtime = Arc::new(ModelFamilySource {
        transport: peer.clone(),
        providers: Some(Arc::new(move |snapshot| {
            let p = get(get(get(get(snapshot, "config"), "model"), "providers"), "a").clone();
            let label = text(get(&p, "url")).rsplit('/').next().unwrap().to_string();
            Ok(vec![Arc::new(OwnedProvider {
                inner: NativeModelInvocationProvider::new(p, transport.clone()),
                trace: provider_trace.clone(),
                label,
            })])
        })),
    });
    let registry = registry(&scratch, families);
    let home = scratch.0.join("home");
    write_config(&home, "old", 1000);
    let old = registry.resolve(None).unwrap();
    let mut release_old = old.acquire().unwrap();
    let old_ports = ports(&old, "old", &trace, &scratch);
    let old_context = turn_context("old");
    let prepared = bounded(old_ports.execution.prepare(PrepareInput {
        request: get(&oracle, "request").clone(),
        context: old_context.clone(),
    }))
    .await
    .unwrap();
    let metadata = old_ports
        .metadata
        .as_ref()
        .unwrap()
        .get_model_max_context_tokens(&"a".into(), &"m".into())
        .unwrap()
        .unwrap();
    push(
        &trace,
        format!(
            "metadata:old:{}",
            stringify_json(&metadata).unwrap().unwrap()
        ),
    );
    let mut old_stream = old_ports
        .execution
        .stream(StreamInput {
            prepared,
            context: old_context,
            abort: None,
        })
        .await
        .unwrap();
    for _ in 0..2 {
        assert!(take(old_stream.as_mut(), "old", &trace).await);
    }
    write_config(&home, "new", 2000);
    bounded(registry.reload()).await.unwrap();
    let fresh = registry.resolve(None).unwrap();
    let mut release_new = fresh.acquire().unwrap();
    let fresh_ports = ports(&fresh, "new", &trace, &scratch);
    for (label, ports, runtime) in [("old", &old_ports, &old), ("new", &fresh_ports, &fresh)] {
        let metadata = ports
            .metadata
            .as_ref()
            .unwrap()
            .get_model_max_context_tokens(&"a".into(), &"m".into())
            .unwrap()
            .unwrap();
        push(
            &trace,
            format!(
                "metadata:{label}:{}",
                stringify_json(&metadata).unwrap().unwrap()
            ),
        );
        assert!(
            context(runtime)
                .resource(Resource::TokenAccounting)
                .is_some()
        );
    }
    for (label, runtime) in [("old", &old), ("new", &fresh)] {
        let resource = model(runtime);
        let providers = &resource
            .as_any()
            .downcast_ref::<ModelResources>()
            .unwrap()
            .providers;
        push(
            &trace,
            format!(
                "generation:{label}:{}",
                stringify_json(&providers.get_provider_generation(&"a".into()))
                    .unwrap()
                    .unwrap()
            ),
        );
    }
    let fresh_context = turn_context("new");
    let abort = pilotdeck_gateway::runtime::model::router::AbortFlag::new();
    let prepared = bounded(fresh_ports.execution.prepare(PrepareInput {
        request: get(&oracle, "request").clone(),
        context: fresh_context.clone(),
    }))
    .await
    .unwrap();
    let mut fresh_stream = fresh_ports
        .execution
        .stream(StreamInput {
            prepared,
            context: fresh_context,
            abort: Some(abort.clone()),
        })
        .await
        .unwrap();
    for _ in 0..2 {
        assert!(take(fresh_stream.as_mut(), "new", &trace).await);
    }
    abort.abort();
    until(|| has(&trace, "http-cancel:new")).await;
    push(&trace, "cancel-observed:true");
    bounded(fresh_stream.close()).await.unwrap();
    push(
        &trace,
        format!("old-disposed-before-drain:{}", has(&trace, "dispose:old")),
    );
    peer.finish("old");
    while take(old_stream.as_mut(), "old", &trace).await {}
    bounded(release_old.release()).await.unwrap();
    push(
        &trace,
        format!("old-disposed-after-drain:{}", has(&trace, "dispose:old")),
    );
    bounded(release_new.release()).await.unwrap();
    bounded(registry.dispose()).await.unwrap();
    assert_eq!(*trace.lock().unwrap(), strings(get(&oracle, "trace")));
}

struct ReplacementProvider {
    id: JsString,
    label: &'static str,
    trace: Trace,
}
impl ModelInvocationProvider for ReplacementProvider {
    fn provider_id(&self) -> &JsString {
        &self.id
    }
    fn dispose(&self) -> DisposeFuture<'_> {
        Box::pin(async {
            push(&self.trace, format!("dispose:{}", self.label));
            Ok(())
        })
    }
}
impl RouterModelInvocation for ReplacementProvider {
    fn get_capabilities(&self, _: &JsString, _: &JsString) -> Result<JsValue, ModuleError> {
        Ok(JsValue::Undefined)
    }
    fn get_multimodal(&self, _: &JsString, _: &JsString) -> Result<JsValue, ModuleError> {
        Ok(JsValue::Undefined)
    }
    fn stream<'a>(
        &'a self,
        _: &'a JsValue,
        options: &'a StreamOptions,
        sink: &'a mut (dyn FnMut(StreamOutput) + Send),
    ) -> RouterStreamFuture<'a> {
        Box::pin(async move {
            push(&self.trace, self.label);
            sink(StreamOutput::Event(JsValue::Undefined));
            options.yield_boundary().await;
            Ok(())
        })
    }
}
#[tokio::test]
async fn provider_replacement_captures_accepted_generation_and_drains() {
    let oracle = baseline::oracle(ORACLE);
    let trace = Trace::default();
    let registry = ModelInvocationProviderRegistry::new(tokio::runtime::Handle::current());
    let make = |label| {
        Arc::new(ReplacementProvider {
            id: "a".into(),
            label,
            trace: trace.clone(),
        }) as Arc<dyn ModelInvocationProvider>
    };
    let old = registry.register(make("old")).unwrap();
    let request = get(&oracle, "request");
    let gate = YieldGate::new();
    gate.deliver();
    let options = StreamOptions {
        yield_gate: Some(gate.clone()),
        ..Default::default()
    };
    let mut sink = |_| {};
    let mut accepted = registry.stream(request, &options, &mut sink);
    let replacement = registry.replace(make("new")).unwrap();
    push(
        &trace,
        format!("current:{}", replacement.registration.generation()),
    );
    for _ in 0..16 {
        tokio::task::yield_now().await;
    }
    push(
        &trace,
        format!("old-disposed:{}", has(&trace, "dispose:old")),
    );
    // First poll starts the already accepted old provider, then parks at yield.
    assert!(matches!(
        std::future::poll_fn(|cx| Poll::Ready(accepted.as_mut().poll(cx))).await,
        Poll::Pending
    ));
    registry
        .stream(request, &StreamOptions::default(), &mut |_| {})
        .await
        .unwrap();
    let expected = strings(get(&oracle, "replacements"));
    assert_eq!(
        trace.lock().unwrap().last(),
        expected.get(3),
        "new invocation must use the published provider"
    );
    gate.pull();
    accepted.await.unwrap();
    bounded(replacement.retired.unwrap().dispose())
        .await
        .unwrap();
    bounded(old.dispose()).await.unwrap();
    push(
        &trace,
        format!(
            "still-current:{}",
            stringify_json(&registry.get_provider_generation(&"a".into()))
                .unwrap()
                .unwrap()
        ),
    );
    bounded(registry.dispose()).await.unwrap();
    assert_eq!(
        *trace.lock().unwrap(),
        strings(get(&oracle, "replacements"))
    );
}

fn error_value(error: ModuleError) -> JsValue {
    let mut value = JsObject::new();
    value.insert("code".into(), JsValue::String(error.code.unwrap()));
    value.insert("message".into(), JsValue::String(error.message));
    JsValue::Object(value)
}

#[tokio::test]
async fn unregister_after_replace_removes_current_generation_matches_ts() {
    let oracle = baseline::oracle(ORACLE);
    let trace = Trace::default();
    let registry = ModelInvocationProviderRegistry::new(tokio::runtime::Handle::current());
    let make = |label| {
        Arc::new(ReplacementProvider {
            id: "a".into(),
            label,
            trace: trace.clone(),
        }) as Arc<dyn ModelInvocationProvider>
    };
    let original = registry.register(make("old")).unwrap();
    let replaced = registry.replace(make("new")).unwrap();
    bounded(replaced.retired.unwrap().dispose()).await.unwrap();
    bounded(original.dispose()).await.unwrap();
    push(
        &trace,
        format!(
            "old-dispose-keeps-current:{}",
            registry.acquire(&"a".into()).unwrap().generation()
        ),
    );
    let held_current = registry.acquire(&"a".into()).unwrap();
    let id = "a".into();
    let removing = registry.unregister(&id);
    tokio::pin!(removing);
    // Poll through the synchronous removal before checking the drain wait.
    // The held lease prevents disposal, independently of scheduler timing.
    let first_poll = std::future::poll_fn(|cx| Poll::Ready(removing.as_mut().poll(cx))).await;
    assert!(registry.get_provider_generation(&id).is_undefined());
    assert!(
        first_poll.is_pending(),
        "unregister must wait for the held lease to drain"
    );
    push(
        &trace,
        format!(
            "unregister-removed-current:{}",
            registry.get_provider_generation(&"a".into()).is_undefined()
        ),
    );
    push(
        &trace,
        format!("new-disposed-before-drain:{}", has(&trace, "dispose:new")),
    );
    registry.register(make("later")).unwrap();
    drop(held_current);
    let removed = tokio::time::timeout(Duration::from_secs(5), removing)
        .await
        .expect("unregister must complete after the held lease drains")
        .unwrap();
    push(&trace, format!("unregister:{removed}"));
    push(
        &trace,
        format!(
            "later-still-current:{}",
            registry.acquire(&"a".into()).unwrap().generation()
        ),
    );
    let removed = bounded(registry.unregister(&"a".into())).await.unwrap();
    push(&trace, format!("unregister:{removed}"));
    let removed = bounded(registry.unregister(&"a".into())).await.unwrap();
    push(&trace, format!("unregister:{removed}"));
    bounded(registry.dispose()).await.unwrap();
    assert_eq!(
        *trace.lock().unwrap(),
        strings(get(&oracle, "unregisterAfterReplace"))
    );
}

#[tokio::test]
async fn non_string_provider_errors_match_ts_without_coercing_route_keys() {
    let oracle = baseline::oracle(ORACLE);
    let JsValue::Array(cases) = get(&oracle, "providerValueCases") else {
        panic!("provider value cases")
    };
    let registry = ModelInvocationProviderRegistry::new(tokio::runtime::Handle::current());
    for case in cases {
        let string_id = get(case, "stringId").as_string().unwrap();
        if registry.acquire(string_id).is_err() {
            registry
                .register(Arc::new(JudgeProvider {
                    id: string_id.clone(),
                }))
                .unwrap();
        }
        let mut request = get(case, "request").clone();
        // JSON omits undefined properties and encodes -0 as 0. Restore the
        // exact JS probe inputs so both typed boundary cases are exercised.
        if let JsValue::Object(object) = &mut request {
            match text(get(case, "name")).as_str() {
                "undefined" => {
                    object.insert("provider".into(), JsValue::Undefined);
                }
                "negative-zero" => {
                    object.insert("provider".into(), JsValue::Number(-0.0));
                }
                _ => {}
            }
        }
        let stream_error = bounded(registry.stream(
            &request,
            &StreamOptions::default(),
            &mut |_| panic!("missing provider emitted an event"),
        ))
        .await
        .unwrap_err();
        assert_eq!(
            error_value(stream_error.into()),
            *get(case, "streamError"),
            "stream case {}",
            text(get(case, "name"))
        );
        let complete_error = bounded(registry.complete(request, None)).await.unwrap_err();
        let mut error = ModuleError::new(complete_error.message);
        error.code = complete_error.code;
        assert_eq!(
            error_value(error),
            *get(case, "completeError"),
            "complete case {}",
            text(get(case, "name"))
        );
        let mut string_request = get(case, "request").as_object().unwrap().clone();
        string_request.insert("provider".into(), JsValue::String(string_id.clone()));
        assert_eq!(
            bounded(registry.complete(JsValue::Object(string_request), None))
                .await
                .unwrap(),
            *get(case, "stringCompletion")
        );
    }
    bounded(registry.dispose()).await.unwrap();
}

struct ApiProvider {
    id: JsString,
    label: &'static str,
    trace: Trace,
    fail: bool,
    entered: Arc<std::sync::atomic::AtomicBool>,
}
impl ModelInvocationProvider for ApiProvider {
    fn provider_id(&self) -> &JsString {
        &self.id
    }
    fn dispose(&self) -> DisposeFuture<'_> {
        Box::pin(async {
            push(&self.trace, self.label);
            if self.fail {
                Err(RuntimeError::new(self.label))
            } else {
                Ok(())
            }
        })
    }
}
impl RouterModelInvocation for ApiProvider {
    fn get_capabilities(&self, _: &JsString, _: &JsString) -> Result<JsValue, ModuleError> {
        Ok(JsValue::Undefined)
    }
    fn get_multimodal(&self, _: &JsString, _: &JsString) -> Result<JsValue, ModuleError> {
        Ok(JsValue::Undefined)
    }
    fn supports_complete(&self) -> bool {
        true
    }
    fn complete(
        &self,
        _: JsValue,
        abort: Option<pilotdeck_gateway::runtime::model::router::AbortFlag>,
    ) -> pilotdeck_gateway::runtime::model::router::RouterFuture<JsValue> {
        let entered = self.entered.clone();
        let trace = self.trace.clone();
        Box::pin(async move {
            push(&trace, "complete-start");
            entered.store(true, std::sync::atomic::Ordering::SeqCst);
            abort.unwrap().cancelled().await;
            push(&trace, "complete-abort");
            Err(pilotdeck_gateway::runtime::model::router::RouterInvokeError::other("cancelled"))
        })
    }
}
#[tokio::test]
async fn registry_errors_ordered_disposal_and_complete_lease_match_ts() {
    let oracle = baseline::oracle(ORACLE);
    let mut api = Vec::new();
    let executor = tokio::runtime::Handle::current();
    let errors = ModelInvocationProviderRegistry::new(executor.clone());
    let make = |id: &str, label, trace: Trace, fail| {
        Arc::new(ApiProvider {
            id: id.into(),
            label,
            trace,
            fail,
            entered: Arc::default(),
        }) as Arc<dyn ModelInvocationProvider>
    };
    for id in ["", " \t", "\u{feff}"] {
        api.push(error_value(
            errors
                .register(make(id, "", Trace::default(), false))
                .err()
                .unwrap(),
        ));
    }
    errors
        .register(make("a", "", Trace::default(), false))
        .unwrap();
    api.push(error_value(
        errors
            .register(make("a", "", Trace::default(), false))
            .err()
            .unwrap(),
    ));
    let mut lease = errors.acquire(&"a".into()).unwrap();
    lease.release();
    lease.release();
    api.push(
        format!("lease-generation:{}", lease.generation())
            .as_str()
            .into(),
    );
    api.push(
        format!(
            "unregister:{}",
            bounded(errors.unregister(&"a".into())).await.unwrap()
        )
        .as_str()
        .into(),
    );
    api.push(
        format!(
            "unregister:{}",
            errors.unregister(&"a".into()).await.unwrap()
        )
        .as_str()
        .into(),
    );
    api.push(error_value(errors.acquire(&"a".into()).err().unwrap()));
    bounded(errors.dispose()).await.unwrap();
    api.push(error_value(
        errors
            .register(make("a", "", Trace::default(), false))
            .err()
            .unwrap(),
    ));
    assert_eq!(JsValue::Array(api), *get(&oracle, "api"));
    let trace = Trace::default();
    let disposal = ModelInvocationProviderRegistry::new(executor.clone());
    disposal
        .register(make("z", "old-z", trace.clone(), true))
        .unwrap();
    let hold = disposal.acquire(&"z".into()).unwrap();
    disposal
        .register(make("a", "a", trace.clone(), true))
        .unwrap();
    let switched = disposal
        .replace(make("z", "new-z", trace.clone(), true))
        .unwrap();
    drop(hold);
    let failed = bounded(switched.retired.unwrap().dispose())
        .await
        .unwrap_err();
    let aggregate = bounded(disposal.dispose()).await.unwrap_err();
    let recorded = get(&oracle, "disposal");
    assert_eq!(*trace.lock().unwrap(), strings(get(recorded, "order")));
    assert_eq!(failed.message, text(get(recorded, "failedOld")));
    assert_eq!(
        aggregate.message,
        text(get(get(recorded, "aggregate"), "message"))
    );
    assert_eq!(
        aggregate
            .causes
            .iter()
            .map(|error| error.message.clone())
            .collect::<Vec<_>>(),
        strings(get(get(recorded, "aggregate"), "causes"))
    );
    // Repeating dispose preserves the exact error and never disposes twice.
    assert_eq!(bounded(disposal.dispose()).await.unwrap_err(), aggregate);
    assert_eq!(*trace.lock().unwrap(), strings(get(recorded, "order")));
    let trace = Trace::default();
    let entered = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let completes = ModelInvocationProviderRegistry::new(executor);
    let registration = completes
        .register(Arc::new(ApiProvider {
            id: "a".into(),
            label: "complete-dispose",
            trace: trace.clone(),
            fail: false,
            entered: entered.clone(),
        }))
        .unwrap();
    let abort = pilotdeck_gateway::runtime::model::router::AbortFlag::new();
    let completion =
        tokio::spawn(completes.complete(get(&oracle, "request").clone(), Some(abort.clone())));
    until(|| entered.load(std::sync::atomic::Ordering::SeqCst)).await;
    let drained = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let flag = drained.clone();
    let drain_registration = registration.clone();
    let drain = tokio::spawn(async move {
        drain_registration.when_drained().await;
        flag.store(true, std::sync::atomic::Ordering::SeqCst);
    });
    let disposing = tokio::spawn(async move { registration.dispose().await });
    tokio::task::yield_now().await;
    push(
        &trace,
        format!(
            "drained-before:{}",
            drained.load(std::sync::atomic::Ordering::SeqCst)
        ),
    );
    abort.abort();
    let failure = bounded(completion).await.unwrap().unwrap_err();
    assert_eq!(
        failure.message.to_utf8().unwrap(),
        text(get(&oracle, "completeError"))
    );
    bounded(drain).await.unwrap();
    bounded(disposing).await.unwrap().unwrap();
    push(
        &trace,
        format!(
            "drained-after:{}",
            drained.load(std::sync::atomic::Ordering::SeqCst)
        ),
    );
    bounded(completes.dispose()).await.unwrap();
    assert_eq!(
        *trace.lock().unwrap(),
        strings(get(&oracle, "completeTrace"))
    );
}

#[tokio::test]
async fn native_default_family_metadata_recording_and_http_abort_match_ts() {
    let oracle = baseline::oracle(ORACLE);
    let scratch = Scratch::new();
    let trace = Trace::default();
    let peer = Arc::new(Peer {
        frames: get(&oracle, "frames").clone(),
        trace: trace.clone(),
        pending: Mutex::default(),
    });
    let mut families = families();
    families.model_runtime = Arc::new(ModelFamilySource {
        transport: peer,
        providers: None,
    });
    let registry = registry(&scratch, families);
    write_config(&scratch.0.join("home"), "native", 3000);
    let runtime = registry.resolve(None).unwrap();
    // Exercise the family call site as well as the pure default projection.
    let resource = router(&runtime);
    let concrete = resource.as_any().downcast_ref::<RouterResources>().unwrap();
    let JsValue::Array(configs) = get(&oracle, "routerConfigs") else {
        panic!("configs")
    };
    let mut config = JsObject::new();
    config.insert("router".into(), get(&configs[0], "result").clone());
    let mut expected = JsObject::new();
    expected.insert("config".into(), JsValue::Object(config));
    assert_eq!(
        *concrete.router.config(),
        pilotdeck_gateway::runtime::model::router::RouterConfig::from_snapshot(&JsValue::Object(
            expected
        ))
    );
    let mut lease = runtime.acquire().unwrap();
    let ports = ports(&runtime, "native", &trace, &scratch);
    let ctx = turn_context("native");
    let prepared = bounded(ports.execution.prepare(PrepareInput {
        request: get(&oracle, "request").clone(),
        context: ctx.clone(),
    }))
    .await
    .unwrap();
    let metadata = ports
        .metadata
        .as_ref()
        .unwrap()
        .get_model_max_context_tokens(&"a".into(), &"m".into())
        .unwrap()
        .unwrap();
    push(
        &trace,
        format!(
            "metadata:native:{}",
            stringify_json(&metadata).unwrap().unwrap()
        ),
    );
    let abort = pilotdeck_gateway::runtime::model::router::AbortFlag::new();
    let mut stream = ports
        .execution
        .stream(StreamInput {
            prepared,
            context: ctx,
            abort: Some(abort.clone()),
        })
        .await
        .unwrap();
    for _ in 0..2 {
        assert!(take(stream.as_mut(), "native", &trace).await);
    }
    abort.abort();
    until(|| has(&trace, "http-cancel:native")).await;
    bounded(stream.close()).await.unwrap();
    bounded(lease.release()).await.unwrap();
    bounded(registry.dispose()).await.unwrap();
    assert_eq!(*trace.lock().unwrap(), strings(get(&oracle, "nativeTrace")));
}

struct JudgeProvider {
    id: JsString,
}
impl ModelInvocationProvider for JudgeProvider {
    fn provider_id(&self) -> &JsString {
        &self.id
    }
}
impl RouterModelInvocation for JudgeProvider {
    fn get_capabilities(&self, _: &JsString, _: &JsString) -> Result<JsValue, ModuleError> {
        Ok(JsValue::Undefined)
    }
    fn get_multimodal(&self, _: &JsString, _: &JsString) -> Result<JsValue, ModuleError> {
        Ok(JsValue::Undefined)
    }
    fn supports_complete(&self) -> bool {
        true
    }
    fn complete(
        &self,
        _: JsValue,
        _: Option<pilotdeck_gateway::runtime::model::router::AbortFlag>,
    ) -> pilotdeck_gateway::runtime::model::router::RouterFuture<JsValue> {
        Box::pin(async { Ok(parse_json(r#"{"label":"judge-b"}"#).unwrap()) })
    }
}

async fn registry_router_case(name: &str) {
    let oracle = baseline::oracle(ORACLE);
    let JsValue::Array(cases) = get(&oracle, "registryRouterCases") else {
        panic!("registry router cases")
    };
    let case = cases
        .iter()
        .find(|case| text(get(case, "name")) == name)
        .unwrap();
    let scratch = Scratch::new();
    let mut families = families();
    if name != "native" {
        let mixed = name == "mixed";
        families.model_runtime = Arc::new(ModelFamilySource {
            providers: Some(Arc::new(move |snapshot| {
                if !mixed {
                    return Ok(vec![]);
                }
                let provider = get(get(get(snapshot, "config"), "model"), "providers")
                    .get("a")
                    .unwrap()
                    .clone();
                let native = NativeModelInvocationProvider::new(
                    provider,
                    Arc::new(pilotdeck_gateway::runtime::model::http::HyperModelTransport::new()),
                );
                assert!(native.supports_complete());
                Ok(vec![
                    Arc::new(native),
                    Arc::new(JudgeProvider { id: "b".into() }),
                ])
            })),
            ..Default::default()
        });
    }
    let registry = registry(&scratch, families);
    let home = scratch.0.join("home");
    write_config(&home, name, 3000);
    let config_path = home.join("pilotdeck.yaml");
    let config = std::fs::read_to_string(&config_path).unwrap();
    std::fs::write(
        config_path,
        config + &text(get(case, "providerYaml")) + &text(get(case, "routerYaml")),
    )
    .unwrap();
    let runtime = registry.resolve(None).unwrap();
    let router = router(&runtime);
    assert_eq!(JsValue::Bool(true), *get(case, "staged"));
    let router = &router
        .as_any()
        .downcast_ref::<RouterResources>()
        .unwrap()
        .router;
    assert!(router.config().enabled);
    let saver = router.config().token_saver.as_ref().unwrap();
    assert!(saver.enabled);
    assert_eq!(
        saver.judge.provider,
        if name == "native" { "a" } else { "b" }.into()
    );
    let model = model(&runtime);
    let providers = &model
        .as_any()
        .downcast_ref::<ModelResources>()
        .unwrap()
        .providers;
    assert_eq!(
        JsValue::Bool(providers.supports_complete()),
        *get(case, "completeAvailable")
    );
    if name != "native" {
        let result = bounded(providers.complete(get(case, "request").clone(), None)).await;
        if let Some(expected) = case.get("completion") {
            assert_eq!(result.unwrap(), *expected);
        } else {
            let error = result.unwrap_err();
            let mut value = JsObject::new();
            value.insert("code".into(), JsValue::String(error.code.unwrap()));
            value.insert("message".into(), JsValue::String(error.message));
            assert_eq!(JsValue::Object(value), *get(case, "error"));
        }
    } else {
        // #604: Native provider now implements complete() via http::complete_model.
        let lease = providers.acquire(&"a".into()).unwrap();
        assert!(lease.provider().supports_complete());
        drop(lease);
        // complete() should now succeed (or fail with HTTP errors, not "not implemented")
        // Full oracle verification is in registryRouterCases; here we only verify
        // supports_complete correctness.
    }
    bounded(registry.dispose()).await.unwrap();
}

#[tokio::test]
async fn mixed_registry_router_staging_and_judge_dispatch_match_ts() {
    registry_router_case("mixed").await;
}
#[tokio::test]
async fn native_registry_enabled_router_staging_matches_ts() {
    registry_router_case("native").await;
}
#[tokio::test]
async fn empty_registry_router_staging_and_missing_judge_match_ts() {
    registry_router_case("empty").await;
}

#[tokio::test]
async fn empty_registry_complete_without_provider_matches_ts() {
    registry_router_case("empty-missing-provider").await;
}

#[tokio::test]
async fn model_family_resolve_without_tokio_returns_error_without_panic() {
    let scratch = Scratch::new();
    let registry = Arc::new(registry(&scratch, families()));
    write_config(&scratch.0.join("home"), "native", 3000);
    let worker_registry = registry.clone();
    let failure = std::thread::spawn(move || worker_registry.resolve(None).err().unwrap())
        .join()
        .expect("synchronous resolve must not panic");
    assert_eq!(
        failure.message,
        "Model family staging requires a Tokio runtime."
    );
    // A failed off-runtime attempt must not publish a broken generation.
    assert!(registry.resolve(None).is_ok());
    bounded(registry.dispose()).await.unwrap();
}
