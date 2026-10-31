//! #777: one Rust test per real-TS M20 cell, also included as unit tests so
//! compile-time mutants exercise the same production paths and assertions.
#![allow(clippy::duplicate_mod)]
#[path = "common/baseline.rs"]
mod baseline;
#[path = "support/llm_stub.rs"]
#[allow(dead_code)]
mod llm_stub;
#[path = "common/scratch.rs"]
mod scratch;

use pilotdeck_gateway::{
    host::Cancellation,
    runtime::{
        context::memory::{
            self, ContextMemoryMessage, MemoryCaptureFuture, MemoryCaptureTurnInput, MemoryFuture,
            MemoryResolver, MemoryRetrieveInput, MemoryRetrieveResult,
        },
        memory::{
            MemoryProviderError,
            edgeclaw::{
                provider::{
                    EdgeClawMemoryProvider, EdgeClawMemoryProviderOptions,
                    EdgeClawMemoryServiceLike, RetrieveContextInput, ServiceRetrieveFuture,
                },
                service::{
                    CaptureTurnInput, EdgeClawMemoryService, ServiceDependencies, ServiceOptions,
                },
            },
            maintenance::MaintenanceTelemetry,
        },
        model::http::{HyperModelTransport, ModelHttpTransport, TransportFuture},
    },
};
use pilotdeck_protocol::{JsObject, JsValue, parse_json};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::Duration,
};
use tokio::sync::{Semaphore, oneshot, watch};
const ORACLE: &str = include_str!("fixtures/edgeclaw-provider/oracle.json");
const NOW: &str = "2026-10-22T00:00:00.000Z";
const NOW_MS: i64 = 1792627200000;

fn js(value: &Value) -> JsValue {
    parse_json(&value.to_string()).expect("#777 JSON input")
}
fn obj(value: &Value) -> JsObject {
    js(value).as_object().expect("#777 object input").clone()
}
fn enc(value: &JsValue) -> Value {
    match value {
        JsValue::Undefined => json!({"$undefined":true}),
        JsValue::Null => Value::Null,
        JsValue::Bool(v) => json!(v),
        JsValue::Number(_) => serde_json::from_str(
            &pilotdeck_protocol::stringify_json(value)
                .expect("#777 encode JS number")
                .expect("#777 number is JSON"),
        )
        .expect("#777 canonical JS number"),
        JsValue::String(v) => json!(v.to_utf8().expect("#777 fixture UTF-8 string")),
        JsValue::Array(v) => Value::Array(v.iter().map(enc).collect()),
        JsValue::Object(v) => Value::Object(
            v.entries()
                .iter()
                .map(|(k, v)| (k.to_utf8().expect("#777 fixture key"), enc(v)))
                .collect(),
        ),
    }
}
fn text<'a>(value: &'a Value, key: &str) -> &'a str {
    value[key]
        .as_str()
        .unwrap_or_else(|| panic!("#777 string {key}"))
}
fn messages() -> Vec<JsValue> {
    js(&json!([
        {"role":"user","content":[{"type":"text","text":"  question  "}]},
        {"role":"assistant","content":[{"type":"text","text":" old answer "}],"metadata":{"forkCarryover":true}},
        {"role":"assistant","content":[{"type":"text","text":" final answer "},{"type":"tool_result","content":[{"type":"text","text":" tool output "}]}]},
        {"role":"assistant","content":[{"type":"text","text":"   "}]}
    ])).as_array_for_fixture()
}
trait FixtureArray {
    fn as_array_for_fixture(&self) -> Vec<JsValue>;
}
impl FixtureArray for JsValue {
    fn as_array_for_fixture(&self) -> Vec<JsValue> {
        match self {
            JsValue::Array(v) => v.clone(),
            _ => panic!("#777 array input"),
        }
    }
}
type Reply = Result<JsObject, MemoryProviderError>;
struct ServiceState {
    calls: Vec<Value>,
    traces: Vec<Value>,
    signals: HashMap<String, Cancellation>,
    gates: HashMap<String, oneshot::Sender<Reply>>,
    capture_error: bool,
    save_error: bool,
    save_enabled: bool,
    closed: bool,
    resolve_in_capture: Option<(String, Reply)>,
}
struct Service {
    state: Mutex<ServiceState>,
    entered: Semaphore,
}
impl Service {
    fn new() -> Self {
        Self {
            state: Mutex::new(ServiceState {
                calls: vec![],
                traces: vec![],
                signals: HashMap::new(),
                gates: HashMap::new(),
                capture_error: false,
                save_error: false,
                save_enabled: true,
                closed: false,
                resolve_in_capture: None,
            }),
            entered: Semaphore::new(0),
        }
    }
    fn finish(&self, query: &str, result: Reply) {
        let tx = self
            .state
            .lock()
            .expect("#777 service lock")
            .gates
            .remove(query)
            .expect("#777 entered retrieval gate");
        tx.send(result)
            .expect("#777 retrieval remains alive after timeout/abort");
    }
}
impl EdgeClawMemoryServiceLike for Service {
    fn retrieve_context(self: Arc<Self>, input: RetrieveContextInput) -> ServiceRetrieveFuture {
        let query = input.query.to_utf8().expect("#777 query");
        let (tx, rx) = oneshot::channel();
        let mut state = self.state.lock().expect("#777 service lock");
        state.calls.push(json!({"method":"retrieve","query":query,"options":{
            "recentMessages":input.recent_messages.iter().map(|m|enc(&m.to_js())).collect::<Vec<_>>(),
            "workspaceHint":input.workspace_hint.to_utf8().expect("#777 workspace"),
            "retrievalMode":input.retrieval_mode.to_utf8().expect("#777 mode"),
            "signal":{"aborted":input.signal.is_cancelled()}
        }}));
        state.signals.insert(query.clone(), input.signal);
        state.gates.insert(query, tx);
        drop(state);
        self.entered.add_permits(1);
        Box::pin(async move { rx.await.expect("#777 service resolve signal") })
    }
    fn capture_turn(
        &self,
        messages: &[ContextMemoryMessage],
        input: CaptureTurnInput,
    ) -> Result<(), MemoryProviderError> {
        let mut state = self.state.lock().expect("#777 service lock");
        state.calls.push(json!({"method":"capture","messages":messages.iter().map(|m|enc(&m.to_js())).collect::<Vec<_>>(),
            "input":{"sessionKey":input.session_key,"timestamp":input.timestamp,"source":input.source}}));
        if let Some((query, result)) = state.resolve_in_capture.take() {
            state
                .gates
                .remove(&query)
                .expect("#777 capture resolution gate")
                .send(result)
                .expect("#777 capture resolution remains alive");
        }
        if state.capture_error || state.closed {
            Err(MemoryProviderError::new("capture failed"))
        } else {
            Ok(())
        }
    }
    fn supports_case_trace(&self) -> bool {
        self.state.lock().expect("#777 service lock").save_enabled
    }
    fn save_case_trace(&self, record: &JsObject) -> Result<(), MemoryProviderError> {
        let mut state = self.state.lock().expect("#777 service lock");
        state.traces.push(enc(&JsValue::Object(record.clone())));
        if state.save_error || state.closed {
            Err(MemoryProviderError::new("save failed"))
        } else {
            Ok(())
        }
    }
}
#[derive(Default)]
struct Telemetry(Mutex<Vec<Value>>);
impl MaintenanceTelemetry for Telemetry {
    fn track_feature_loop_stage(&self, input: JsValue) {
        self.0
            .lock()
            .expect("#777 telemetry lock")
            .push(json!({"method":"stage","event":enc(&input)}));
    }
    fn track_error(&self, error: &MemoryProviderError, input: JsValue) {
        self.0.lock().expect("#777 telemetry lock").push(json!({"method":"error","error":{"name":error.name,"message":error.message.to_utf8().expect("#777 error message")},"event":enc(&input)}));
    }
}
struct Resolver {
    provider: EdgeClawMemoryProvider,
    results: Mutex<HashMap<String, watch::Receiver<Option<JsValue>>>>,
}
impl Resolver {
    fn start(&self, input: MemoryRetrieveInput) -> MemoryFuture {
        let query = input.query.clone();
        let future = self.provider.retrieve(input);
        let (tx, mut rx) = watch::channel(None);
        self.results
            .lock()
            .expect("#777 result lock")
            .insert(query, rx.clone());
        tokio::spawn(async move {
            tx.send_replace(Some(future.await));
        });
        Box::pin(async move {
            rx.changed().await.expect("#777 provider settled");
            let value = rx.borrow().clone().expect("#777 provider result");
            // Adapter projection is checked independently below. This tap
            // retains the full metadata while using the real builder.
            let wire = enc(&value);
            Ok(MemoryRetrieveResult {
                system_context: wire["systemContext"].as_str().map(str::to_owned),
                diagnostics: wire["diagnostics"]
                    .as_array()
                    .expect("#777 diagnostics")
                    .iter()
                    .map(|d| memory::MemoryDiagnostic {
                        code: text(d, "code").into(),
                        severity: text(d, "severity").into(),
                        message: text(d, "message").into(),
                    })
                    .collect(),
            })
        })
    }
    async fn result(&self, query: &str) -> Value {
        let mut rx = self
            .results
            .lock()
            .expect("#777 result lock")
            .get(query)
            .expect("#777 registered result")
            .clone();
        if rx.borrow().is_none() {
            rx.changed().await.expect("#777 late provider completion");
        }
        let result = rx.borrow().clone().expect("#777 final provider result");
        enc(&result)
    }
}
impl MemoryResolver for Resolver {
    fn retrieve(&self, input: MemoryRetrieveInput) -> MemoryFuture {
        self.start(input)
    }
    fn capture_turn(&self, input: MemoryCaptureTurnInput) -> MemoryCaptureFuture {
        self.provider.capture_turn(input)
    }
}
fn retrieve_input(query: &str, session: &str, signal: Cancellation) -> MemoryRetrieveInput {
    MemoryRetrieveInput {
        query: query.into(),
        session_id: session.into(),
        project_root: "/workspace".into(),
        recent_messages: messages(),
        signal,
    }
}
fn capture_input(session: &str, errored: JsValue) -> MemoryCaptureTurnInput {
    MemoryCaptureTurnInput {
        session_id: session.into(),
        project_root: "/workspace".into(),
        messages: messages(),
        errored,
    }
}
async fn observe(spec: &Value) -> Value {
    let service = Arc::new(Service::new());
    let telemetry = Arc::new(Telemetry::default());
    let now = Arc::new(Mutex::new(NOW.to_owned()));
    let clock = now.clone();
    let mut options = EdgeClawMemoryProviderOptions::new(service.clone());
    options.now = Some(Arc::new(move || {
        clock.lock().expect("#777 clock lock").clone()
    }));
    options.telemetry = Some(telemetry.clone());
    options.source = spec["options"]["source"].as_str().map(str::to_owned);
    options.retrieval_mode = spec["options"]["retrievalMode"].as_str().map(Into::into);
    let resolver = Arc::new(Resolver {
        provider: EdgeClawMemoryProvider::new(options, tokio::runtime::Handle::current()),
        results: Mutex::new(HashMap::new()),
    });
    let parent = Cancellation::default();
    let mut results = vec![];
    let mut building = None;
    if spec["pending"] == true {
        drop(resolver.start(retrieve_input("seed", "s1", parent.clone())));
        service.finish(
            "seed",
            Ok(obj(&json!({"context":" seed context ","intent":"seed"}))),
        );
        resolver.result("seed").await;
        service
            .entered
            .acquire()
            .await
            .expect("#777 seed entered")
            .forget();
    }
    for op in spec["ops"].as_array().expect("#777 operations") {
        match text(op, "op") {
            "begin" => {
                drop(resolver.start(retrieve_input(
                    text(op, "query"),
                    text(op, "session"),
                    parent.clone(),
                )));
                service
                    .entered
                    .acquire()
                    .await
                    .expect("#777 direct entered")
                    .forget();
            }
            "resolve" | "reject" => {
                let query = text(op, "query");
                service.finish(
                    query,
                    if op["op"] == "resolve" {
                        Ok(obj(&op["value"]))
                    } else {
                        Err(MemoryProviderError::new(text(op, "error")))
                    },
                );
                results.push(json!({"query":query,"value":resolver.result(query).await}));
            }
            "resolveInCapture" => {
                service
                    .state
                    .lock()
                    .expect("#777 service lock")
                    .resolve_in_capture = Some((text(op, "query").into(), Ok(obj(&op["value"]))));
            }
            "awaitResult" => {
                let query = text(op, "query");
                results.push(json!({"query":query,"value":resolver.result(query).await}));
            }
            "capture" => resolver
                .provider
                .capture_turn(capture_input(text(op, "session"), js(&op["errored"])))
                .await
                .expect("#777 capture must swallow service errors"),
            "captureOutcome" => {
                let outcome = resolver
                    .provider
                    .capture_turn(capture_input(text(op, "session"), js(&op["errored"])))
                    .await;
                results.push(json!({"captureError":outcome.err()}));
            }
            "captureError" => {
                service
                    .state
                    .lock()
                    .expect("#777 service lock")
                    .capture_error = op["value"] == true
            }
            "saveError" => {
                service.state.lock().expect("#777 service lock").save_error = op["value"] == true
            }
            "saveEnabled" => {
                service
                    .state
                    .lock()
                    .expect("#777 service lock")
                    .save_enabled = op["value"] == true
            }
            "builder" => {
                let resolver = resolver.clone();
                let parent = parent.clone();
                let input = retrieve_input(
                    text(op, "query"),
                    text(op, "session"),
                    Cancellation::default(),
                );
                let timeout = if op["timeout"] == true { 1234.0 } else { 0.0 };
                let pre_aborted = parent.is_cancelled();
                building = Some(tokio::spawn(async move {
                    let (attachment, diagnostics) =
                        memory::build(resolver.as_ref(), input, Some(&parent), timeout).await;
                    let attachments = attachment
                        .into_iter()
                        .map(|text| json!({"role":"user","content":[{"type":"text","text":text}]}))
                        .collect::<Vec<_>>();
                    json!({"attachments":attachments,"diagnostics":diagnostics.into_iter().map(|d|json!({"code":d.code,"severity":d.severity,"message":d.message})).collect::<Vec<_>>()})
                }));
                if !pre_aborted {
                    service
                        .entered
                        .acquire()
                        .await
                        .expect("#777 builder entered")
                        .forget();
                }
            }
            "timeout" | "abort" | "awaitBuilder" => {
                if op["op"] == "timeout" {
                    tokio::time::advance(Duration::from_millis(1234)).await;
                }
                if op["op"] == "abort" {
                    parent.cancel(None);
                }
                results.push(json!({"builder":building.take().expect("#777 builder exists").await.expect("#777 builder completion")}));
            }
            "preAbort" => parent.cancel(None),
            "dispose" => {
                let mut state = service.state.lock().expect("#777 service lock");
                state.closed = true;
                state.calls.push(json!({"method":"close"}));
            }
            "tick" => *now.lock().expect("#777 clock lock") = text(op, "value").into(),
            other => panic!("#777 unknown operation {other}"),
        }
    }
    let state = service.state.lock().expect("#777 service lock");
    let signals = state
        .signals
        .iter()
        .map(|(q, s)| (q.clone(), json!(s.is_cancelled())))
        .collect::<serde_json::Map<_, _>>();
    json!({"spec":spec,"providerHasDispose":false,"calls":state.calls,"traces":state.traces,
        "telemetry":*telemetry.0.lock().expect("#777 telemetry lock"),"results":results,"finalSignals":signals})
}
async fn check_cell(name: &str) {
    let oracle: Value = serde_json::from_str(ORACLE).expect("#777 oracle JSON");
    let cell = oracle["scenarios"]
        .as_array()
        .expect("#777 scenarios")
        .iter()
        .find(|c| c["spec"]["name"] == name)
        .expect("#777 registered matrix cell");
    let observed = tokio::time::timeout(Duration::from_secs(30), observe(&cell["spec"]))
        .await
        .expect("#777 matrix signal did not arrive");
    assert_eq!(observed, *cell, "#777 provider cell {name}");
}
// The same entries emit the test functions and their coverage denominator.
// Removing an entry therefore cannot silently leave the oracle unexercised.
macro_rules! cells {
    ($($test:ident => $name:literal),+ $(,)?) => {
        const REGISTERED_CELLS: &[&str] = &[$($name),+];
        $(
            #[tokio::test(start_paused = true)]
            async fn $test() {
                check_cell($name).await;
            }
        )+
    };
}
cells! {
    none_resolve => "none-resolve",
    none_reject => "none-reject",
    none_empty => "none-empty",
    none_capture => "none-capture",
    none_errored => "none-errored",
    none_capture_error => "none-capture-error",
    none_save_error => "none-save-error",
    none_save_missing => "none-save-missing",
    none_overwrite => "none-overwrite",
    none_reverse => "none-reverse",
    none_capture_inflight => "none-capture-inflight",
    none_resolve_in_capture => "none-resolve-in-capture",
    none_timeout_late => "none-timeout-late",
    none_abort_late => "none-abort-late",
    none_never_abort => "none-never-abort",
    none_dispose_late => "none-dispose-late",
    pending_resolve => "pending-resolve",
    pending_reject => "pending-reject",
    pending_empty => "pending-empty",
    pending_capture => "pending-capture",
    pending_errored => "pending-errored",
    pending_capture_error => "pending-capture-error",
    pending_save_error => "pending-save-error",
    pending_save_missing => "pending-save-missing",
    pending_overwrite => "pending-overwrite",
    pending_reverse => "pending-reverse",
    pending_capture_inflight => "pending-capture-inflight",
    pending_resolve_in_capture => "pending-resolve-in-capture",
    pending_timeout_late => "pending-timeout-late",
    pending_abort_late => "pending-abort-late",
    pending_never_abort => "pending-never-abort",
    pending_dispose_late => "pending-dispose-late",
    sessions => "sessions",
    empty_precedence => "empty-precedence",
    context_fallback => "context-fallback",
    explicit_options => "explicit-options",
    pre_aborted => "pre-aborted",
    direct_pre_aborted => "direct-pre-aborted",
    clock_boundaries => "clock-boundaries",
    malformed_context => "malformed-context",
    malformed_capture => "malformed-capture",
}

#[test]
fn oracle_scenario_count() {
    let oracle: Value = serde_json::from_str(ORACLE).expect("#777 denominator oracle JSON");
    let scenarios = oracle["scenarios"]
        .as_array()
        .expect("#777 denominator scenarios array");
    let expected_count = if pilotdeck_gateway::mutants::on("provider-oracle-count") {
        40
    } else {
        41
    };
    assert_eq!(
        scenarios.len(),
        expected_count,
        "#777 oracle scenario count"
    );

    let mut registered = REGISTERED_CELLS.to_vec();
    if pilotdeck_gateway::mutants::on("provider-missing-cell") {
        registered.pop();
    }
    if pilotdeck_gateway::mutants::on("provider-wrong-cell") {
        *registered
            .first_mut()
            .expect("#777 denominator registered cell") = "unregistered-cell";
    }
    let unique = registered.iter().collect::<std::collections::BTreeSet<_>>();
    assert_eq!(unique.len(), 41, "#777 Rust cell registration count");
    let mut oracle_names = scenarios
        .iter()
        .map(|cell| {
            cell["spec"]["name"]
                .as_str()
                .expect("#777 denominator scenario name")
        })
        .collect::<Vec<_>>();
    registered.sort_unstable();
    oracle_names.sort_unstable();
    assert_eq!(
        registered, oracle_names,
        "#777 Rust cell registrations match oracle"
    );
}

#[tokio::test]
async fn resolver_adapter() {
    let service = Arc::new(Service::new());
    let provider = EdgeClawMemoryProvider::new(
        EdgeClawMemoryProviderOptions::new(service.clone()),
        tokio::runtime::Handle::current(),
    );
    let future = MemoryResolver::retrieve(
        &provider,
        retrieve_input("q1", "s1", Cancellation::default()),
    );
    service.finish("q1", Ok(obj(&json!({"systemContext":" context "}))));
    let result = future.await.expect("#777 resolver projection");
    assert_eq!(
        result.system_context.as_deref(),
        Some("context"),
        "#777 resolver context projection"
    );
    assert!(
        result.diagnostics.is_empty(),
        "#777 resolver diagnostics projection"
    );
}

struct SettledTelemetry(Arc<Semaphore>);
impl MaintenanceTelemetry for SettledTelemetry {
    fn track_feature_loop_stage(&self, input: JsValue) {
        if input
            .get("phase")
            .and_then(JsValue::as_string)
            .is_some_and(|v| v.equals_str("retrieve"))
            && input
                .get("loopStage")
                .and_then(JsValue::as_string)
                .is_some_and(|v| v.equals_str("loop_end"))
        {
            self.0.add_permits(1);
        }
    }
    fn track_error(&self, _error: &MemoryProviderError, _input: JsValue) {
        self.0.add_permits(1);
    }
}
async fn direct_builder_lifecycle(timeout: bool) {
    let service = Arc::new(Service::new());
    let settled = Arc::new(Semaphore::new(0));
    let mut options = EdgeClawMemoryProviderOptions::new(service.clone());
    options.now = Some(Arc::new(|| NOW.into()));
    options.telemetry = Some(Arc::new(SettledTelemetry(settled.clone())));
    let provider = Arc::new(EdgeClawMemoryProvider::new(
        options,
        tokio::runtime::Handle::current(),
    ));
    let parent = Cancellation::default();
    let builder_parent = parent.clone();
    let builder_provider = provider.clone();
    let builder = tokio::spawn(async move {
        memory::build(
            builder_provider.as_ref(),
            retrieve_input("q1", "s1", Cancellation::default()),
            Some(&builder_parent),
            if timeout { 1234.0 } else { 0.0 },
        )
        .await
    });
    service
        .entered
        .acquire()
        .await
        .expect("#777 direct builder service entered")
        .forget();
    if timeout {
        tokio::time::advance(Duration::from_millis(1234)).await;
    } else {
        parent.cancel(None);
    }
    let (attachment, diagnostics) = builder.await.expect("#777 direct builder completion");
    assert!(
        attachment.is_none(),
        "#777 direct builder has no early context"
    );
    assert_eq!(
        diagnostics.len(),
        usize::from(timeout),
        "#777 direct builder cancellation diagnostics"
    );
    provider
        .capture_turn(capture_input("s1", JsValue::Bool(false)))
        .await
        .expect("#777 direct early capture");
    let oracle: Value = serde_json::from_str(ORACLE).expect("#777 direct oracle JSON");
    let cell = oracle["scenarios"]
        .as_array()
        .expect("#777 direct scenarios")
        .iter()
        .find(|c| {
            c["spec"]["name"]
                == if timeout {
                    "none-timeout-late"
                } else {
                    "none-abort-late"
                }
        })
        .expect("#777 direct TS cell");
    let resolution = cell["spec"]["ops"]
        .as_array()
        .expect("#777 direct operations")
        .iter()
        .find(|o| o["op"] == "resolve")
        .expect("#777 direct resolution");
    // This send fails immediately if dropping the builder killed the actual
    // provider work. No proxy task keeps its wait alive in this test.
    service.finish("q1", Ok(obj(&resolution["value"])));
    settled
        .acquire()
        .await
        .expect("#777 direct pending published")
        .forget();
    provider
        .capture_turn(capture_input("s1", JsValue::Bool(false)))
        .await
        .expect("#777 direct late capture");
    assert_eq!(
        json!(
            service
                .state
                .lock()
                .expect("#777 direct service lock")
                .traces
        ),
        cell["traces"],
        "#777 direct builder late trace"
    );
}
#[tokio::test(start_paused = true)]
async fn direct_timeout_keeps_retrieval_alive() {
    tokio::time::timeout(Duration::from_secs(30), direct_builder_lifecycle(true))
        .await
        .expect("#777 timeout lifecycle signal did not arrive");
}
#[tokio::test(start_paused = true)]
async fn direct_abort_keeps_retrieval_alive() {
    tokio::time::timeout(Duration::from_secs(30), direct_builder_lifecycle(false))
        .await
        .expect("#777 abort lifecycle signal did not arrive");
}

struct GatedHttp {
    inner: HyperModelTransport,
    entered: Arc<Semaphore>,
    release: Arc<Semaphore>,
    cancelled: Arc<AtomicBool>,
}
struct Abandoned(Arc<AtomicBool>, bool);
impl Drop for Abandoned {
    fn drop(&mut self) {
        if self.1 {
            self.0.store(true, Ordering::SeqCst);
        }
    }
}
impl ModelHttpTransport for GatedHttp {
    fn send(&self, request: hyper::Request<http_body_util::Full<bytes::Bytes>>) -> TransportFuture {
        let inner = self.inner.clone();
        let entered = self.entered.clone();
        let release = self.release.clone();
        let cancelled = self.cancelled.clone();
        TransportFuture::io(async move {
            let mut guard = Abandoned(cancelled, true);
            entered.add_permits(1);
            release
                .acquire()
                .await
                .expect("#777 release real HTTP")
                .forget();
            let response = inner.send(request).await;
            guard.1 = false;
            response
        })
    }
}
async fn real_service_observation(close_early: bool) -> Value {
    let oracle: Value = serde_json::from_str(ORACLE).expect("#777 oracle JSON");
    let expected = &oracle[if close_early {
        "realServiceClose"
    } else {
        "realServiceAbort"
    }];
    let server = llm_stub::LlmStub::start(
        expected["script"].as_array().expect("#777 script").clone(),
        Arc::new(AtomicU64::new(NOW_MS as u64)),
    )
    .await;
    let _closed = server.closed.clone();
    let scratch = scratch::Scratch::new("777-provider");
    let entered = Arc::new(Semaphore::new(0));
    let release = Arc::new(Semaphore::new(0));
    let cancelled = Arc::new(AtomicBool::new(false));
    let mut options = ServiceOptions::new(scratch.path().join("workspace"));
    options.root_dir = Some(scratch.path().join("store"));
    let llm = pilotdeck_gateway::runtime::memory::edgeclaw::llm::config::LlmOptions {
        provider: Some("p".into()),
        model: Some("m".into()),
        model_ref: Some("p/m".into()),
        base_url: Some(format!("{}/v1", server.origin).into()),
        api_key: Some("fixture-key".into()),
        api_type: Some("openai-completions".into()),
        headers: None,
    };
    let config = pilotdeck_gateway::runtime::memory::edgeclaw::llm::config::build_llm_config_with(
        Some(&llm),
        None,
        &|_| None,
    );
    let service = Arc::new(
        EdgeClawMemoryService::with_dependencies(
            options,
            ServiceDependencies {
                home: Some(scratch.path().join("home")),
                clock: Some(Arc::new(|| NOW.into())),
                now_ms: Some(Arc::new(|| NOW_MS)),
                random_token: Some(Arc::new(|| "9".into())),
                math_random: Some(Arc::new(|| 0.25)),
                llm_config: Some(config),
                transport: Some(Arc::new(GatedHttp {
                    inner: HyperModelTransport::new(),
                    entered: entered.clone(),
                    release: release.clone(),
                    cancelled: cancelled.clone(),
                })),
                ..Default::default()
            },
        )
        .expect("#777 real service open"),
    );
    let mut options = EdgeClawMemoryProviderOptions::new(service.clone());
    options.now = Some(Arc::new(|| NOW.into()));
    let provider = EdgeClawMemoryProvider::new(options, tokio::runtime::Handle::current());
    let parent = Cancellation::default();
    let mut input = retrieve_input("remember my preference", "s1", parent.clone());
    input.recent_messages.clear();
    let mut pending = provider.retrieve(input);
    if close_early {
        service.close().expect("#777 immediate service close");
    }
    tokio::select! {permit=entered.acquire()=>permit.expect("#777 real LLM entered").forget(), value=&mut pending=>panic!("#777 real LLM skipped: {value:?}")}
    parent.cancel(None);
    release.add_permits(1);
    let result = tokio::time::timeout(Duration::from_secs(20), pending)
        .await
        .expect("#777 real LLM completes after parent abort");
    let requests = server.requests.lock().expect("#777 HTTP requests").clone();
    let mut observed = json!({"script":expected["script"],"parentAborted":parent.is_cancelled(),"llmSignalAbortedAtParentAbort":cancelled.load(Ordering::SeqCst),"requests":requests,"result":enc(&result)});
    if close_early {
        observed["closedBeforeDispatch"] = json!(true);
    } else {
        service.close().expect("#777 service close");
    }
    server.close().await;
    observed
}
#[tokio::test]
async fn real_service_ignores_parent_abort() {
    baseline::oracle(ORACLE);
    let oracle: Value = serde_json::from_str(ORACLE).expect("#777 oracle JSON");
    assert_eq!(
        real_service_observation(false).await,
        oracle["realServiceAbort"],
        "#777 real service HTTP bytes and abort result"
    );
}

#[tokio::test]
async fn native_service_starts_before_immediate_close() {
    let oracle: Value = serde_json::from_str(ORACLE).expect("#777 oracle JSON");
    assert_eq!(
        real_service_observation(true).await,
        oracle["realServiceClose"],
        "#777 native service starts synchronously before close"
    );
}

#[tokio::test]
#[ignore = "requires fixed TS baseline and Node v22.22.0"]
async fn real_ts_reads_rust_provider_observations() {
    baseline::oracle(ORACLE);
    let oracle: Value = serde_json::from_str(ORACLE).expect("#777 oracle JSON");
    let mut scenarios = vec![];
    tokio::time::pause();
    for cell in oracle["scenarios"].as_array().expect("#777 scenarios") {
        scenarios.push(observe(&cell["spec"]).await);
    }
    tokio::time::resume();
    let observed = json!({"scenarios":scenarios,"realServiceAbort":real_service_observation(false).await,"realServiceClose":real_service_observation(true).await});
    let scratch = scratch::Scratch::new("777-reverse");
    let output = scratch.path().join("observations.json");
    std::fs::write(
        &output,
        serde_json::to_vec(&observed).expect("#777 serialize observations"),
    )
    .expect("#777 save observations");
    let baseline = std::env::var("PDRS_BASELINE").expect("#777 PDRS_BASELINE");
    let result = baseline::node_command()
        .args([
            "--import",
            &format!("{baseline}/node_modules/tsx/dist/loader.mjs"),
        ])
        .arg(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/edgeclaw-provider/verify.mts"
        ))
        .arg(&output)
        .output()
        .expect("#777 execute TS verifier");
    assert!(
        result.status.success(),
        "#777 real TS reverse verification:\n{}{}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
    println!("{}", String::from_utf8_lossy(&result.stdout));
}
