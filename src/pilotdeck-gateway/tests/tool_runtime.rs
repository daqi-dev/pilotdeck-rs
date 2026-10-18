#[path = "common/baseline.rs"]
mod baseline;
use pilotdeck_gateway::{
    host::{
        Cancellation, HostError, HostFuture, ModuleError,
        modules::{
            capability::{
                RuntimeContext, ToolCall, ToolDefinition, ToolExecution, ToolInvocationContext,
            },
            permission::PermissionDecision,
            tool_context::{ExecutionContext, HostToolCheckpoint, ToolRuntimeContext},
        },
    },
    runtime::tools::{
        ExecutableTool, SchedulerMode, ToolCancellation, ToolError, ToolFuture, ToolOutput,
        ToolRegistry, ToolRunContext, ToolRuntime, registry::UnavailableTool,
    },
};
use pilotdeck_protocol::{JsObject, JsString, JsValue, parse_json, stringify_json};
use std::{
    future::Future,
    pin::Pin,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    task::Poll,
    time::Duration,
};
use tokio::time::Instant;

const ORACLE: &str = include_str!("fixtures/tool-runtime/oracle.json");
const EPOCH: f64 = 1_767_225_600_000.0;
static UNDEFINED: JsValue = JsValue::Undefined;
fn field<'a>(v: &'a JsValue, key: &str) -> &'a JsValue {
    v.get(key).unwrap_or(&UNDEFINED)
}
fn text(v: &JsValue) -> String {
    v.as_string().unwrap().to_utf8().unwrap()
}
fn array(v: &JsValue) -> &[JsValue] {
    if let JsValue::Array(v) = v {
        v
    } else {
        panic!("expected array: {v:?}")
    }
}
fn obj<const N: usize>(entries: [(&str, JsValue); N]) -> JsObject {
    let mut o = JsObject::new();
    for (k, v) in entries {
        o.insert(k.into(), v);
    }
    o
}
fn value(s: impl Into<JsString>) -> JsValue {
    JsValue::String(s.into())
}
fn wire(v: &JsValue) -> String {
    stringify_json(v).unwrap().unwrap()
}
fn equal(a: &JsValue, b: &JsValue, label: &str) {
    assert_eq!(wire(a), wire(b), "{label}");
}

#[derive(Clone)]
struct Probe {
    started: Instant,
    events: Arc<Mutex<Vec<JsValue>>>,
    live: Arc<AtomicUsize>,
}
impl Probe {
    fn new() -> Self {
        Self {
            started: Instant::now(),
            events: Arc::default(),
            live: Arc::default(),
        }
    }
    fn ms(&self) -> f64 {
        self.started.elapsed().as_millis() as f64
    }
    fn event(&self, event: &str, id: &JsValue) {
        self.events.lock().unwrap().push(JsValue::Array(vec![
            event.into(),
            id.clone(),
            JsValue::Number(self.ms()),
        ]));
    }
}
struct Live(Arc<AtomicUsize>);
impl Live {
    fn new(probe: &Probe) -> Self {
        probe.live.fetch_add(1, Ordering::SeqCst);
        Self(probe.live.clone())
    }
}
impl Drop for Live {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}
struct FakeTool {
    fields: JsObject,
    spec: JsValue,
    probe: Probe,
}
impl ToolDefinition for FakeTool {
    fn fields(&self) -> &JsObject {
        &self.fields
    }
    fn is_read_only(&self, _: &JsValue) -> Result<JsValue, HostError> {
        Ok(self
            .spec
            .get("readOnly")
            .cloned()
            .unwrap_or(JsValue::Bool(true)))
    }
    fn is_concurrency_safe(&self, input: &JsValue) -> Result<JsValue, HostError> {
        if field(&self.spec, "predicateError") == &JsValue::Bool(true) {
            return Err(ModuleError::new("predicate failed").into());
        }
        Ok(input
            .get("safe")
            .cloned()
            .unwrap_or_else(|| field(&self.spec, "safe").clone()))
    }
    fn requires_user_interaction(&self, _: &JsValue) -> Result<JsValue, HostError> {
        Ok(self
            .spec
            .get("interaction")
            .cloned()
            .unwrap_or(JsValue::Bool(false)))
    }
}
impl ExecutableTool for FakeTool {
    fn validate_input(
        &self,
        input: JsValue,
        _: ToolRunContext,
    ) -> ToolFuture<Option<Vec<JsObject>>> {
        Box::pin(async move {
            if field(&input, "validationError") == &JsValue::Bool(true) {
                return Err(ToolError::new(
                    "path_not_allowed",
                    "Path validation failed.",
                ));
            }
            Ok(
                if let JsValue::Array(issues) = field(&input, "validationIssues") {
                    Some(
                        issues
                            .iter()
                            .map(|v| v.as_object().unwrap().clone())
                            .collect(),
                    )
                } else {
                    None
                },
            )
        })
    }
    fn execute(&self, input: JsValue, context: ToolRunContext) -> ToolFuture<ToolOutput> {
        let probe = self.probe.clone();
        Box::pin(async move {
            let _live = Live::new(&probe);
            let id = context.fields.get("currentToolCallId").unwrap().clone();
            probe.event("start", &id);
            if field(&input, "hang") == &JsValue::Bool(true) {
                return std::future::pending().await;
            }
            let ignore_abort = field(&input, "ignoreAbort") == &JsValue::Bool(true);
            let delay = field(&input, "delay").as_number().unwrap_or(0.0);
            let budget = if field(&input, "useBudget") == &JsValue::Bool(true) {
                context
                    .fields
                    .get("subagentTimeoutMs")
                    .and_then(JsValue::as_number)
            } else {
                None
            };
            let wait = delay.min(budget.unwrap_or(f64::INFINITY));
            let timer = async {
                if wait == 0.0 {
                    tokio::task::yield_now().await;
                } else {
                    tokio::time::sleep(Duration::from_secs_f64(wait / 1000.0)).await;
                }
            };
            let result = tokio::select! { biased;
                () = async { if ignore_abort { std::future::pending().await } else { context.cancelled().await } } => Err(ToolError::aborted()),
                () = timer => {
                    if budget.is_some_and(|v| v < delay) { Err(ToolError::new("tool_timeout", "Budget expired.")) }
                    else if field(&input, "fail").as_string().is_some() {
                        match text(field(&input, "fail")).as_str() {
                            "Error" => Err(ToolError::failed("ordinary failure")),
                            "nonError" => Err(ToolError::non_error()),
                            code => { let mut e = ToolError::new(code, "fake failure"); e.details = Some(obj([("stderr", "ENOENT detail".into())])); Err(e) }
                        }
                    } else { Ok(()) }
                }
            };
            if let Err(error) = result {
                probe.event("error", &id);
                return Err(error);
            }
            probe.event("end", &id);
            Ok(ToolOutput {
                content: match input.get("content") {
                    Some(content) => array(content).to_vec(),
                    None => vec![JsValue::Object(obj([
                        ("type", "text".into()),
                        ("text", input.get("value").cloned().unwrap_or(id)),
                    ]))],
                },
                data: input.get("data").cloned(),
                metadata: input.get("metadata").and_then(JsValue::as_object).cloned(),
                supplemental_messages: input.get("supplementalMessages").map(|v| array(v).to_vec()),
            })
        })
    }
}
struct Permission;
impl PermissionDecision for Permission {
    fn decide(
        &self,
        _: Arc<dyn ToolDefinition>,
        input: JsValue,
        _: RuntimeContext,
        _: JsString,
    ) -> HostFuture<JsValue> {
        Box::pin(async move {
            if input
                .get("decision")
                .and_then(JsValue::as_string)
                .is_some_and(|s| s.equals_str("throw"))
            {
                return Err(ModuleError::new("permission failed").into());
            }
            Ok(JsValue::Object(obj([
                (
                    "type",
                    input
                        .get("decision")
                        .cloned()
                        .unwrap_or_else(|| "allow".into()),
                ),
                (
                    "reason",
                    JsValue::Object(obj([
                        ("type", "runtime".into()),
                        (
                            "message",
                            input
                                .get("permissionMessage")
                                .cloned()
                                .unwrap_or_else(|| "fixture".into()),
                        ),
                    ])),
                ),
                ("message", "permission stopped".into()),
                (
                    "request",
                    JsValue::Object(obj([("options", JsValue::Array(vec!["allow".into()]))])),
                ),
                (
                    "updatedInput",
                    input
                        .get("updatedInput")
                        .cloned()
                        .unwrap_or(JsValue::Undefined),
                ),
            ])))
        })
    }
}
/// Tools must observe either signal type the host may store.
#[derive(Clone)]
enum Signal {
    Tool(ToolCancellation),
    Host(Cancellation),
}
impl Signal {
    fn cancel(&self) {
        match self {
            Signal::Tool(s) => s.cancel(),
            Signal::Host(s) => s.cancel(None),
        }
    }
    fn runtime(&self) -> RuntimeContext {
        match self {
            Signal::Tool(s) => Arc::new(s.clone()),
            Signal::Host(s) => Arc::new(s.clone()),
        }
    }
}
fn context(
    probe: &Probe,
    cancel: &ToolCancellation,
    patch: Option<&JsObject>,
) -> Arc<ToolRuntimeContext> {
    context_with(probe, &Signal::Tool(cancel.clone()), patch)
}
fn context_with(
    probe: &Probe,
    signal: &Signal,
    patch: Option<&JsObject>,
) -> Arc<ToolRuntimeContext> {
    let clock = probe.clone();
    let mut fields = obj([
        ("cwd", "/workspace".into()),
        ("sessionId", "s".into()),
        ("turnId", "t".into()),
        ("permissionMode", "default".into()),
        (
            "permissionContext",
            JsValue::Object(obj([
                ("mode", "default".into()),
                ("canPrompt", JsValue::Bool(true)),
                ("additionalWorkingDirectories", JsValue::Array(vec![])),
            ])),
        ),
    ]);
    if let Some(patch) = patch {
        for (key, v) in patch.entries() {
            fields.insert(key.clone(), v.clone());
        }
    }
    Arc::new(ToolRuntimeContext {
        fields,
        files: HostToolCheckpoint::default().tool_context_state(),
        abort_signal: Some(signal.runtime()),
        model: None,
        elicitation: None,
        user_dialog: None,
        audit_recorder: None,
        file_history: None,
        file_update_notifier: None,
        now: Some(Arc::new(move || JsValue::Number(EPOCH + clock.ms()))),
        progress: None,
        subagent: None,
        plan_todo: None,
        goal: None,
        plan_directory: None,
    })
}
/// `schemaPrototypes: [{path, prototype}]` sets the prototype of the object at
/// `path` (null for Object.create(null), an array or object otherwise), the
/// same construction the TS generator applies.
fn with_prototypes(mut schema: JsValue, prototypes: &JsValue) -> JsValue {
    fn set(value: &mut JsValue, path: &[JsValue], prototype: &JsValue) {
        let JsValue::Object(object) = value else {
            panic!("prototype path must reach an object")
        };
        match path.split_first() {
            None => object.set_prototype(prototype.clone()),
            Some((key, rest)) => {
                let key = key.as_string().unwrap().clone();
                let mut child = object.get_key(&key).unwrap().clone();
                set(&mut child, rest, prototype);
                object.insert(key, child);
            }
        }
    }
    if let JsValue::Array(prototypes) = prototypes {
        for p in prototypes {
            set(&mut schema, array(field(p, "path")), field(p, "prototype"));
        }
    }
    schema
}
fn register(registry: &ToolRegistry, spec: &JsValue, probe: &Probe) {
    let mut fields = obj([
        ("name", field(spec, "name").clone()),
        ("description", field(spec, "name").clone()),
        ("aliases", field(spec, "aliases").clone()),
        ("kind", "builtin".into()),
        (
            "inputSchema",
            match spec.get("schemaJson") {
                Some(json) => with_prototypes(
                    parse_json(&text(json)).unwrap(),
                    field(spec, "schemaPrototypes"),
                ),
                None => spec
                    .get("schema")
                    .cloned()
                    .unwrap_or_else(|| JsValue::Object(obj([("type", "object".into())]))),
            },
        ),
    ]);
    if let Some(limit) = spec.get("maxResultBytes") {
        fields.insert("maxResultBytes".into(), limit.clone());
    }
    registry
        .register(Arc::new(FakeTool {
            fields,
            spec: spec.clone(),
            probe: probe.clone(),
        }))
        .unwrap();
}
fn calls(values: &JsValue) -> Vec<ToolCall> {
    array(values)
        .iter()
        .map(|v| ToolCall {
            id: field(v, "id").as_string().unwrap().clone(),
            name: field(v, "name").as_string().unwrap().clone(),
            // Raw JSON keeps own `__proto__` keys and non-sorted integer keys.
            input: match v.get("inputJson") {
                Some(json) => parse_json(&text(json)).unwrap(),
                None => field(v, "input").clone(),
            },
        })
        .collect()
}

struct Replay {
    result: Result<Vec<JsObject>, HostError>,
    events: Vec<JsValue>,
    settled_at: f64,
    live: usize,
}
/// Run one recorded scenario through the Rust runtime on a paused clock.
async fn replay(oracle: &JsValue, spec: &JsValue, signal: Signal) -> Replay {
    let probe = Probe::new();
    let cancel = signal;
    if field(spec, "preCancel") == &JsValue::Bool(true) {
        cancel.cancel();
    }
    let cancel_task = field(spec, "cancelAt").as_number().map(|at| {
        let cancel = cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_secs_f64(at / 1000.0)).await;
            cancel.cancel();
        })
    });
    let registry = ToolRegistry::default();
    for def in array(
        spec.get("definitions")
            .unwrap_or(field(oracle, "definitions")),
    ) {
        register(&registry, def, &probe);
    }
    if let JsValue::Array(hidden) = field(spec, "hidden") {
        for name in hidden {
            registry.hide(name.as_string().unwrap());
        }
    }
    if let JsValue::Array(unavailable) = field(spec, "unavailable") {
        for v in unavailable {
            registry
                .mark_unavailable(
                    UnavailableTool {
                        name: field(v, "toolName").as_string().unwrap().clone(),
                        code: field(v, "code").as_string().unwrap().clone(),
                        reason: field(v, "reason").as_string().unwrap().clone(),
                    },
                    &array(field(v, "aliases"))
                        .iter()
                        .map(|v| v.as_string().unwrap().clone())
                        .collect::<Vec<_>>(),
                )
                .unwrap();
        }
    }
    let mode = if field(spec, "mode")
        .as_string()
        .is_some_and(|s| s.equals_str("sequential"))
    {
        SchedulerMode::Sequential
    } else {
        SchedulerMode::Concurrent
    };
    let runtime = ToolRuntime::new(registry, Arc::new(Permission), mode);
    let mut execution = JsObject::new();
    if let Some(ms) = field(spec, "deadline").as_number() {
        execution.insert(
            "operationDeadline".into(),
            value(pilotdeck_transport::system_clock::format_epoch_iso(
                (EPOCH + ms) as i64,
            )),
        );
    }
    let result = runtime
        .execute_all(
            calls(field(spec, "calls")),
            ToolInvocationContext {
                runtime: context_with(&probe, &cancel, field(spec, "context").as_object()),
                execution: Arc::new(ExecutionContext {
                    fields: execution,
                    abort_signal: None,
                }),
            },
        )
        .await;
    let settled_at = probe.ms();
    if let Some(task) = cancel_task {
        task.await.unwrap();
    }
    let events = probe.events.lock().unwrap().clone();
    Replay {
        result,
        events,
        settled_at,
        live: probe.live.load(Ordering::SeqCst),
    }
}

#[tokio::test(start_paused = true)]
async fn real_ts_runtime_schedules_results_errors_and_deadline_budgets() {
    let oracle = baseline::oracle(ORACLE);
    let scenarios = array(field(&oracle, "scenarios"));
    assert_eq!(scenarios.len(), 64);
    // Scenarios flagged rustSerializesNonReadOnly pin a deliberate deviation
    // and are asserted explicitly in non_readonly_safe_tools_run_sequentially.
    for (spec, signal) in scenarios
        .iter()
        .filter(|spec| field(spec, "rustSerializesNonReadOnly").is_undefined())
        .flat_map(|spec| {
            [
                (spec, Signal::Tool(ToolCancellation::default())),
                (spec, Signal::Host(Cancellation::default())),
            ]
        })
    {
        let label = format!(
            "{} ({})",
            text(field(spec, "name")),
            match signal {
                Signal::Tool(_) => "ToolCancellation",
                Signal::Host(_) => "host Cancellation",
            }
        );
        let Replay {
            result,
            events,
            settled_at,
            live,
        } = replay(&oracle, spec, signal).await;
        match result {
            Ok(_) if !field(spec, "error").is_undefined() => panic!(
                "{label}: TS rejected with {:?}, Rust resolved",
                text(field(spec, "error"))
            ),
            Ok(results) => equal(
                &JsValue::Array(results.into_iter().map(JsValue::Object).collect()),
                field(spec, "results"),
                &label,
            ),
            Err(e) => assert_eq!(e.message, text(field(spec, "error")), "{label}"),
        }
        let mut expected = array(field(spec, "events")).to_vec();
        if field(spec, "drain") == &JsValue::Bool(true) {
            // Deliberate deviation (fixtures/tool-runtime/README.md): the
            // rejection time is TS's exactly, but the started siblings that
            // TS lets finish detached after it are dropped by the batch.
            let ts_settled = field(spec, "settledAt").as_number().unwrap();
            assert_eq!(settled_at, ts_settled, "{label}: rejection time");
            let after: Vec<_> = expected
                .iter()
                .filter(|e| array(e)[2].as_number().unwrap() > ts_settled)
                .collect();
            assert!(!after.is_empty(), "{label}: TS siblings ran on");
            assert!(
                after.iter().all(|e| text(&array(e)[0]) == "end"),
                "{label}: only sibling completions follow the TS rejection"
            );
            expected.retain(|e| array(e)[2].as_number().unwrap() <= ts_settled);
        }
        equal(
            &JsValue::Array(events),
            &JsValue::Array(expected),
            &format!("{label} events"),
        );
        assert_eq!(live, 0, "{label}: leaked tool future");
    }
}

fn strip_times(results: &JsValue) -> JsValue {
    JsValue::Array(
        array(results)
            .iter()
            .map(|r| {
                let mut r = r.as_object().unwrap().clone();
                r.insert("startedAt".into(), JsValue::Undefined);
                r.insert("completedAt".into(), JsValue::Undefined);
                JsValue::Object(r)
            })
            .collect(),
    )
}
fn events(json: &str) -> JsValue {
    parse_json(json).unwrap()
}
/// Deliberate deviation (fixtures/tool-runtime/README.md): a concurrencySafe
/// tool that is not read-only (TS agent.ts:166-167) runs concurrently in TS
/// but in the sequential phase here, so a concurrent error never drops it.
#[tokio::test(start_paused = true)]
async fn non_readonly_safe_tools_run_sequentially() {
    let oracle = baseline::oracle(ORACLE);
    let flagged: Vec<_> = array(field(&oracle, "scenarios"))
        .iter()
        .filter(|s| field(s, "rustSerializesNonReadOnly") == &JsValue::Bool(true))
        .collect();
    assert_eq!(flagged.len(), 2);
    for signal in [
        Signal::Tool(ToolCancellation::default()),
        Signal::Host(Cancellation::default()),
    ] {
        // 1. No error: same results apart from timing; TS starts all four at
        // t=0, Rust runs the read-only pair first, then each agent call.
        let spec = flagged[0];
        assert_eq!(
            text(field(spec, "name")),
            "non-readonly-safe-tools-run-concurrently-in-ts"
        );
        let ts_starts: Vec<_> = array(field(spec, "events"))
            .iter()
            .filter(|e| text(&array(e)[0]) == "start")
            .map(|e| array(e)[2].as_number().unwrap())
            .collect();
        assert_eq!(ts_starts, [0.0; 4], "TS runs the agent calls concurrently");
        let run = replay(&oracle, spec, signal.clone()).await;
        let results = JsValue::Array(
            run.result
                .unwrap()
                .into_iter()
                .map(JsValue::Object)
                .collect(),
        );
        equal(
            &strip_times(&results),
            &strip_times(field(spec, "results")),
            "results apart from timing",
        );
        equal(
            &JsValue::Array(run.events),
            &events(
                r#"[["start","b",0],["start","d",0],["end","d",1],["end","b",5],["start","a",5],["end","a",8],["start","c",8],["end","c",10]]"#,
            ),
            "Rust serializes the non-read-only safe calls",
        );
        assert_eq!(run.settled_at, 10.0);
        assert_eq!(run.live, 0);

        // 2. A read-only sibling errors: TS rejects at t=0 and the agent call
        // still runs to completion; Rust rejects at t=0 too, and the agent
        // call, being in the sequential phase, never starts (nothing cut short).
        let spec = flagged[1];
        assert_eq!(
            text(field(spec, "name")),
            "non-readonly-safe-tool-outlives-concurrent-error-in-ts"
        );
        equal(
            field(spec, "events"),
            &events(r#"[["start","a",0],["start","c",0],["end","c",2],["end","a",3]]"#),
            "TS lets the agent call finish after rejecting",
        );
        let run = replay(&oracle, spec, signal).await;
        assert_eq!(run.result.unwrap_err().message, text(field(spec, "error")));
        assert_eq!(
            run.settled_at,
            field(spec, "settledAt").as_number().unwrap()
        );
        equal(
            &JsValue::Array(run.events),
            &events(r#"[["start","c",0]]"#),
            "agent never started",
        );
        assert_eq!(run.live, 0);
    }
}

async fn poll_once<F: Future>(future: Pin<&mut F>) -> Poll<F::Output> {
    let mut future = future;
    std::future::poll_fn(|cx| Poll::Ready(future.as_mut().poll(cx))).await
}
fn hanging(probe: &Probe) -> (ToolRuntime, Vec<ToolCall>) {
    let registry = ToolRegistry::default();
    register(
        &registry,
        &parse_json(r#"{"name":"read_file","safe":true}"#).unwrap(),
        probe,
    );
    let calls = calls(&parse_json(r#"[{"id":"a","name":"read_file","input":{"hang":true}},{"id":"b","name":"read_file","input":{"hang":true}}]"#).unwrap());
    (
        ToolRuntime::new(registry, Arc::new(Permission), SchedulerMode::Concurrent),
        calls,
    )
}
#[tokio::test(start_paused = true)]
async fn dropping_batch_reclaims_all_noncooperative_tool_futures_immediately() {
    let probe = Probe::new();
    let cancel = ToolCancellation::default();
    let (runtime, calls) = hanging(&probe);
    let mut batch =
        Box::pin(runtime.execute_batch(calls, ToolRunContext::new(context(&probe, &cancel, None))));
    assert!(poll_once(batch.as_mut()).await.is_pending());
    assert_eq!(probe.live.load(Ordering::SeqCst), 2);
    drop(batch);
    assert_eq!(probe.live.load(Ordering::SeqCst), 0);
}
#[tokio::test(start_paused = true)]
async fn abort_and_outer_operation_deadline_leave_no_detached_futures() {
    let probe = Probe::new();
    let cancel = ToolCancellation::default();
    let (runtime, calls) = hanging(&probe);
    let mut batch = Box::pin(runtime.execute_batch(
        calls.clone(),
        ToolRunContext::new(context(&probe, &cancel, None)),
    ));
    assert!(poll_once(batch.as_mut()).await.is_pending());
    // Abort is cooperative as in TS: started tools that ignore the signal are
    // not abandoned with a synthetic result, but stay owned by the batch.
    cancel.cancel();
    tokio::time::advance(Duration::from_secs(60)).await;
    assert!(poll_once(batch.as_mut()).await.is_pending());
    assert_eq!(probe.live.load(Ordering::SeqCst), 2);
    drop(batch);
    assert_eq!(probe.live.load(Ordering::SeqCst), 0);
    let signal = ToolCancellation::default();
    let timeout = tokio::time::timeout(
        Duration::from_millis(10),
        runtime.execute_batch(calls, ToolRunContext::new(context(&probe, &signal, None))),
    );
    assert!(timeout.await.is_err());
    assert_eq!(probe.live.load(Ordering::SeqCst), 0);
    let before = probe.events.lock().unwrap().len();
    tokio::time::advance(Duration::from_secs(60)).await;
    assert_eq!(probe.events.lock().unwrap().len(), before);
}

/// Deliberate deviation from Promise.all: the first concurrent error rejects
/// at once (as in TS) and the batch drops the started siblings instead of
/// leaving them running, so a non-cooperative sibling cannot delay or hang it.
#[tokio::test(start_paused = true)]
async fn concurrent_error_rejects_immediately_and_drops_started_siblings() {
    let probe = Probe::new();
    let cancel = ToolCancellation::default();
    let (runtime, _) = hanging(&probe);
    let calls = calls(&parse_json(r#"[{"id":"a","name":"read_file","input":{"decision":"throw"}},{"id":"b","name":"read_file","input":{"hang":true}},{"id":"c","name":"read_file","input":{"hang":true}}]"#).unwrap());
    let mut batch =
        Box::pin(runtime.execute_batch(calls, ToolRunContext::new(context(&probe, &cancel, None))));
    let Poll::Ready(Err(error)) = poll_once(batch.as_mut()).await else {
        panic!("first error must reject the batch on the same poll");
    };
    assert_eq!(error.message, "permission failed");
    assert_eq!(probe.ms(), 0.0);
    let started: Vec<_> = probe
        .events
        .lock()
        .unwrap()
        .iter()
        .map(|e| text(&array(e)[1]))
        .collect();
    assert_eq!(started, ["b", "c"], "siblings had started");
    drop(batch);
    assert_eq!(
        probe.live.load(Ordering::SeqCst),
        0,
        "started siblings dropped"
    );
}

/// Tool destructors and visibility policies may query the same registry; the
/// registry must release its lock before either runs.
struct Reentrant {
    fields: JsObject,
    registry: ToolRegistry,
    drops: Arc<AtomicUsize>,
}
impl ToolDefinition for Reentrant {
    fn fields(&self) -> &JsObject {
        &self.fields
    }
    fn is_read_only(&self, _: &JsValue) -> Result<JsValue, HostError> {
        Ok(JsValue::Bool(true))
    }
    fn is_concurrency_safe(&self, _: &JsValue) -> Result<JsValue, HostError> {
        Ok(JsValue::Bool(true))
    }
}
impl ExecutableTool for Reentrant {
    fn execute(&self, _: JsValue, _: ToolRunContext) -> ToolFuture<ToolOutput> {
        Box::pin(async { Ok(ToolOutput::default()) })
    }
}
impl Drop for Reentrant {
    fn drop(&mut self) {
        let _ = self.registry.list_all();
        let _ = self.registry.has(&"x".into());
        self.drops.fetch_add(1, Ordering::SeqCst);
    }
}
#[test]
fn registry_releases_its_lock_before_tool_destructors_and_policies_run() {
    let drops = Arc::new(AtomicUsize::new(0));
    let registry = ToolRegistry::default();
    let tool = |name: &str| {
        Arc::new(Reentrant {
            fields: obj([("name", name.into()), ("aliases", JsValue::Array(vec![]))]),
            registry: registry.clone(),
            drops: drops.clone(),
        })
    };
    registry.register(tool("x")).unwrap();
    registry.replace(tool("x")).unwrap();
    assert_eq!(drops.load(Ordering::SeqCst), 1);
    let second = registry.register_or_replace(tool("y")).unwrap();
    registry.register_or_replace(tool("y")).unwrap();
    assert_eq!(drops.load(Ordering::SeqCst), 2);
    assert!(!second.is_active());
    assert!(registry.register(tool("x")).is_err());
    assert_eq!(drops.load(Ordering::SeqCst), 3);
    assert!(registry.unregister(&"x".into()).unwrap());
    assert_eq!(drops.load(Ordering::SeqCst), 4);
    let handle = registry.register(tool("z")).unwrap();
    handle.dispose();
    assert_eq!(drops.load(Ordering::SeqCst), 5);
    let inner = registry.clone();
    let scoped = registry
        .scoped(Arc::new(move |t: &dyn ExecutableTool| {
            !inner.is_hidden(t.fields().get("name").unwrap().as_string().unwrap())
                && inner.has(&"y".into())
        }))
        .unwrap();
    assert_eq!(scoped.list().unwrap().len(), 1);
    assert!(scoped.get(&"y".into()).is_some());
    registry.dispose();
    assert_eq!(drops.load(Ordering::SeqCst), 6);
}
