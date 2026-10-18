use pilotdeck_gateway::host::{
    HostError, HostFuture, HostHandler, ModuleError,
    modules::{
        ModuleRouter,
        capability::{
            self, CapabilityResultObserver, PlanTodoHandler, RuntimeContext, ToolCall, ToolCatalog,
            ToolContext, ToolDefinition, ToolExecution, ToolInvocationContext,
        },
        permission::{self, PermissionContext, PermissionDecision},
    },
};
use pilotdeck_protocol::{JsObject, JsString, JsValue, parse_json, stringify_json};
use std::sync::{Arc, Mutex};
use tokio::sync::Notify;

fn obj<const N: usize>(fields: [(&str, JsValue); N]) -> JsObject {
    let mut result = JsObject::new();
    for (key, value) in fields {
        result.insert(key.into(), value);
    }
    result
}
fn val<const N: usize>(fields: [(&str, JsValue); N]) -> JsValue {
    JsValue::Object(obj(fields))
}
fn field(value: &JsValue, key: &str) -> JsValue {
    value.get(key).cloned().unwrap_or(JsValue::Undefined)
}
fn array(value: &JsValue) -> &[JsValue] {
    if let JsValue::Array(items) = value {
        items
    } else {
        panic!("expected array: {value:?}")
    }
}
fn normalized(value: &JsValue) -> JsValue {
    parse_json(&stringify_json(value).unwrap().unwrap()).unwrap()
}
fn basic() -> JsValue {
    parse_json(r#"{"fields":{"name":"read","description":"Read fixture","kind":"builtin","inputSchema":{"type":"object"},"private":"catalog-only"}}"#).unwrap()
}
fn record(trace: &Mutex<Vec<JsValue>>, event: &str, fields: JsObject) {
    let mut result = obj([("event", event.into())]);
    for (key, value) in fields.entries() {
        result.insert(key.clone(), value.clone());
    }
    trace
        .lock()
        .unwrap()
        .push(normalized(&JsValue::Object(result)));
}

struct Definition {
    fields: JsObject,
    flags: JsValue,
    capabilities: Option<Vec<JsString>>,
    trace: Arc<Mutex<Vec<JsValue>>>,
}
impl Definition {
    fn predicate(&self, key: &str, input: &JsValue) -> Result<JsValue, HostError> {
        assert_eq!(input, &val([]));
        record(
            &self.trace,
            "predicate",
            obj([
                ("name", self.fields.get("name").cloned().unwrap()),
                ("predicate", key.into()),
                ("input", input.clone()),
            ]),
        );
        let Some(flag) = self.flags.get(key) else {
            return Ok(JsValue::Bool(false));
        };
        if flag.get("throws").and_then(JsValue::as_bool) == Some(true) {
            return Err(ModuleError::new("predicate failed").into());
        }
        Ok(field(flag, "value"))
    }
}
impl ToolDefinition for Definition {
    fn fields(&self) -> &JsObject {
        &self.fields
    }
    fn is_read_only(&self, input: &JsValue) -> Result<JsValue, HostError> {
        self.predicate("readOnly", input)
    }
    fn is_concurrency_safe(&self, input: &JsValue) -> Result<JsValue, HostError> {
        self.predicate("concurrencySafe", input)
    }
    fn requires_user_interaction(&self, input: &JsValue) -> Result<JsValue, HostError> {
        if self.flags.get("requiresUserInteraction").is_none() {
            Ok(JsValue::Bool(false))
        } else {
            self.predicate("requiresUserInteraction", input)
        }
    }
    fn required_runtime_capabilities(&self) -> Option<Vec<JsString>> {
        self.capabilities.clone()
    }
}

#[derive(Clone)]
struct Fake {
    config: JsValue,
    tools: Arc<Mutex<Vec<Arc<dyn ToolDefinition>>>>,
    trace: Arc<Mutex<Vec<JsValue>>>,
    observer_gate: Option<Arc<Notify>>,
}
impl Fake {
    fn new(config: JsValue) -> Self {
        let result = Self {
            config,
            tools: Arc::new(Mutex::new(Vec::new())),
            trace: Arc::new(Mutex::new(Vec::new())),
            observer_gate: None,
        };
        let definitions = result
            .config
            .get("tools")
            .cloned()
            .unwrap_or_else(|| JsValue::Array(vec![basic()]));
        *result.tools.lock().unwrap() = result.definitions(&definitions);
        result
    }
    fn definitions(&self, definitions: &JsValue) -> Vec<Arc<dyn ToolDefinition>> {
        array(definitions)
            .iter()
            .map(|definition| {
                Arc::new(Definition {
                    fields: definition
                        .get("fields")
                        .unwrap()
                        .as_object()
                        .unwrap()
                        .clone(),
                    flags: field(definition, "flags"),
                    capabilities: definition.get("capabilities").map(|items| {
                        array(items)
                            .iter()
                            .map(|v| v.as_string().unwrap().clone())
                            .collect()
                    }),
                    trace: self.trace.clone(),
                }) as Arc<dyn ToolDefinition>
            })
            .collect()
    }
    fn record(&self, event: &str, fields: JsObject) {
        record(&self.trace, event, fields);
    }
    fn fail(&self, point: &str) -> Result<(), HostError> {
        if self
            .config
            .get("failure")
            .and_then(JsValue::as_string)
            .is_some_and(|v| v.equals_str(point))
        {
            Err(ModuleError::new(format!("fixture {point} failed")).into())
        } else {
            Ok(())
        }
    }
    fn router(&self) -> ModuleRouter {
        let port = Arc::new(self.clone());
        ModuleRouter {
            capability: capability::Handler::new(port.clone(), port.clone(), port.clone())
                .with_plan_todo(port.clone()),
            permission: permission::Handler::new(
                Arc::new(Catalog(self.clone())),
                port.clone(),
                port,
            ),
            ..ModuleRouter::default()
        }
    }
}
impl ToolCatalog for Fake {
    fn list(&self) -> HostFuture<Vec<Arc<dyn ToolDefinition>>> {
        let this = self.clone();
        Box::pin(async move {
            tokio::task::yield_now().await;
            this.record("list", obj([]));
            this.fail("list")?;
            Ok(this.tools.lock().unwrap().clone())
        })
    }
}
struct Catalog(Fake);
impl ToolCatalog for Catalog {
    fn list(&self) -> HostFuture<Vec<Arc<dyn ToolDefinition>>> {
        let this = self.0.clone();
        Box::pin(async move {
            tokio::task::yield_now().await;
            this.record("catalog", obj([]));
            this.fail("catalog")?;
            Ok(this.tools.lock().unwrap().clone())
        })
    }
}
impl ToolExecution for Fake {
    fn refresh(&self) -> HostFuture<()> {
        let this = self.clone();
        Box::pin(async move {
            this.record("refresh", obj([]));
            tokio::task::yield_now().await;
            this.fail("refresh")?;
            if let Some(definitions) = this.config.get("refreshedTools") {
                *this.tools.lock().unwrap() = this.definitions(definitions);
            }
            Ok(())
        })
    }
    fn execute_all(
        &self,
        calls: Vec<ToolCall>,
        context: ToolInvocationContext,
    ) -> HostFuture<Vec<JsObject>> {
        let this = self.clone();
        Box::pin(async move {
            let trace_calls = calls
                .iter()
                .map(|call| {
                    val([
                        ("id", JsValue::String(call.id.clone())),
                        ("name", JsValue::String(call.name.clone())),
                        ("input", call.input.clone()),
                    ])
                })
                .collect();
            this.record(
                "execute",
                obj([
                    ("calls", JsValue::Array(trace_calls)),
                    (
                        "context",
                        context.runtime.downcast_ref::<JsValue>().unwrap().clone(),
                    ),
                    (
                        "execution",
                        context.execution.downcast_ref::<JsValue>().unwrap().clone(),
                    ),
                ]),
            );
            tokio::task::yield_now().await;
            this.fail("execute")?;
            if let Some(results) = this.config.get("results") {
                return Ok(array(results)
                    .iter()
                    .map(|v| v.as_object().unwrap().clone())
                    .collect());
            }
            Ok(calls
                .into_iter()
                .map(|call| {
                    let is_error = call.name.equals_str("unknown");
                    obj([
                        ("toolCallId", JsValue::String(call.id)),
                        ("name", JsValue::String(call.name)),
                        ("result", call.input),
                        ("isError", JsValue::Bool(is_error)),
                    ])
                })
                .collect())
        })
    }
}
impl ToolContext for Fake {
    fn prepare(&self, call: JsValue) -> HostFuture<ToolInvocationContext> {
        let this = self.clone();
        Box::pin(async move {
            let plan_todo = val([("sessionId", "session".into())]);
            this.record("plan-session", plan_todo.as_object().unwrap().clone());
            let runtime = obj([
                ("value", field(call.get("payload").unwrap(), "context")),
                ("planTodo", plan_todo),
                ("includeOneShotSubagent", JsValue::Bool(true)),
            ]);
            this.record("context", runtime.clone());
            this.fail("context")?;
            this.record("execution-context", obj([("call", call.clone())]));
            let execution = val([
                ("requestId", field(&call, "requestId")),
                ("runId", field(&call, "runId")),
                ("operationId", field(&call, "operationId")),
            ]);
            Ok(ToolInvocationContext {
                runtime: Arc::new(JsValue::Object(runtime)),
                execution: Arc::new(execution),
            })
        })
    }
}
impl CapabilityResultObserver for Fake {
    fn on_capability_results(&self, results: Vec<JsObject>) -> HostFuture<()> {
        let this = self.clone();
        Box::pin(async move {
            this.record(
                "observer",
                obj([(
                    "results",
                    JsValue::Array(results.into_iter().map(JsValue::Object).collect()),
                )]),
            );
            if let Some(gate) = &this.observer_gate {
                gate.notified().await;
            }
            tokio::task::yield_now().await;
            this.fail("observer")
        })
    }
}
impl PlanTodoHandler for Fake {
    fn call(&self, call: JsValue) -> HostFuture<JsObject> {
        let this = self.clone();
        Box::pin(async move {
            this.record("plan", obj([("call", call.clone())]));
            tokio::task::yield_now().await;
            this.fail("plan")?;
            Ok(obj([("forwarded", field(&call, "payload"))]))
        })
    }
}
impl PermissionContext for Fake {
    fn prepare(&self, value: JsValue) -> HostFuture<RuntimeContext> {
        let this = self.clone();
        Box::pin(async move {
            let context = obj([("value", value)]);
            this.record("permission-context", context.clone());
            this.fail("permission-context")?;
            Ok(Arc::new(JsValue::Object(context)) as RuntimeContext)
        })
    }
}
impl PermissionDecision for Fake {
    fn decide(
        &self,
        tool: Arc<dyn ToolDefinition>,
        input: JsValue,
        context: RuntimeContext,
        tool_call_id: JsString,
    ) -> HostFuture<JsValue> {
        let this = self.clone();
        Box::pin(async move {
            assert!(
                this.tools
                    .lock()
                    .unwrap()
                    .iter()
                    .any(|candidate| Arc::ptr_eq(candidate, &tool))
            );
            let mut fields = tool.fields().clone();
            if let Some(capabilities) = tool.required_runtime_capabilities() {
                fields.insert(
                    "requiredRuntimeCapabilities".into(),
                    JsValue::Array(capabilities.into_iter().map(JsValue::String).collect()),
                );
            }
            this.record(
                "decide",
                obj([
                    ("tool", JsValue::Object(fields)),
                    ("input", input.clone()),
                    (
                        "context",
                        context.downcast_ref::<JsValue>().unwrap().clone(),
                    ),
                    ("toolCallId", JsValue::String(tool_call_id)),
                ]),
            );
            tokio::task::yield_now().await;
            this.fail("decide")?;
            Ok(val([
                ("behavior", "allow".into()),
                (
                    "fixture",
                    tool.fields()
                        .get("private")
                        .cloned()
                        .unwrap_or(JsValue::Undefined),
                ),
                ("input", input),
            ]))
        })
    }
}

#[tokio::test]
async fn real_ts_capability_permission_responses_and_port_calls() {
    let oracle = parse_json(include_str!("fixtures/host-capability/oracle.json")).unwrap();
    let cases = array(oracle.get("cases").unwrap());
    assert_eq!(cases.len(), 128);
    for case in cases {
        let fake = Fake::new(field(case, "config"));
        let router = fake.router();
        let response = router.module_call(field(case, "call"));
        // Merely constructing a callback future must not call any domain port.
        assert!(fake.trace.lock().unwrap().is_empty());
        let response = match response.await {
            Ok(payload) => val([
                ("ok", JsValue::Bool(true)),
                ("payload", JsValue::Object(payload)),
            ]),
            Err(error) => val([
                ("ok", JsValue::Bool(false)),
                (
                    "error",
                    val([(
                        "message",
                        JsValue::String(error.module_error.unwrap().message),
                    )]),
                ),
            ]),
        };
        assert_eq!(
            &normalized(&response),
            case.get("response").unwrap(),
            "response: {:?}",
            case.get("name")
        );
        assert_eq!(
            &JsValue::Array(fake.trace.lock().unwrap().clone()),
            case.get("trace").unwrap(),
            "trace: {:?}",
            case.get("name")
        );
        router.dispose().await.unwrap();
    }
}

#[tokio::test]
async fn missing_tool_execution_port_fails_closed_after_payload_validation() {
    let router = ModuleRouter::default();
    for (payload, message) in [
        (
            r#"{"operation":"list_tools"}"#,
            "Host did not provide a tool execution port.",
        ),
        (
            r#"{"operation":"execute","toolCallId":"id","name":"read"}"#,
            "Host did not provide a tool execution port.",
        ),
        (
            r#"{"operation":"execute_batch","calls":[]}"#,
            "Host did not provide a tool execution port.",
        ),
        (
            r#"{"operation":"execute_batch","calls":[{"toolCallId":"id","name":"read"}]}"#,
            "Host did not provide a tool execution port.",
        ),
        (
            r#"{"operation":"execute"}"#,
            "Sidecar payload field toolCallId must be a non-empty string.",
        ),
        (
            r#"{"operation":"execute_batch"}"#,
            "Capability batch call must contain calls.",
        ),
        (
            r#"{"operation":"unknown"}"#,
            "Unsupported sidecar capability operation: unknown",
        ),
        (
            r#"{"operation":"plan_todo"}"#,
            "Host did not provide a plan/todo capability.",
        ),
        (
            "null",
            "Sidecar payload field operation must be a non-empty string.",
        ),
    ] {
        let error = router
            .module_call(val([
                ("module", "capability".into()),
                ("payload", parse_json(payload).unwrap()),
            ]))
            .await
            .unwrap_err();
        assert!(
            error.module_error.unwrap().message.equals_str(message),
            "payload: {payload}"
        );
    }
}

#[tokio::test(flavor = "current_thread")]
async fn observer_is_awaited_without_blocking_other_callbacks() {
    let mut fake = Fake::new(val([]));
    let gate = Arc::new(Notify::new());
    fake.observer_gate = Some(gate.clone());
    let router = Arc::new(fake.router());
    let running = router.clone();
    let mut task = tokio::spawn(async move {
        running
            .module_call(
                parse_json(
                    r#"{"module":"capability","payload":{"operation":"execute_batch","calls":[]}}"#,
                )
                .unwrap(),
            )
            .await
    });
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            if fake.trace.lock().unwrap().iter().any(|e| {
                e.get("event")
                    .and_then(JsValue::as_string)
                    .is_some_and(|s| s.equals_str("observer"))
            }) {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(!task.is_finished());
    // A current-thread runtime can still service another module call.
    let response = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        router.module_call(
            parse_json(r#"{"module":"capability","payload":{"operation":"list_tools"}}"#).unwrap(),
        ),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(array(response.get("tools").unwrap()).len(), 1);
    assert!(!task.is_finished());
    gate.notify_one();
    tokio::time::timeout(std::time::Duration::from_secs(2), &mut task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
}

// Domain state intentionally cannot be serialized: notification identity and
// shared mutation must survive context construction and both dispatch paths.
struct DomainState {
    signal: Notify,
    accesses: std::sync::atomic::AtomicUsize,
}
#[derive(Clone)]
struct DomainPort {
    state: Arc<DomainState>,
    catalog: Fake,
}
impl ToolCatalog for DomainPort {
    fn list(&self) -> HostFuture<Vec<Arc<dyn ToolDefinition>>> {
        self.catalog.list()
    }
}
impl ToolContext for DomainPort {
    fn prepare(&self, _call: JsValue) -> HostFuture<ToolInvocationContext> {
        let state = self.state.clone();
        Box::pin(async move {
            Ok(ToolInvocationContext {
                runtime: state.clone(),
                execution: state,
            })
        })
    }
}
impl ToolExecution for DomainPort {
    fn execute_all(
        &self,
        _calls: Vec<ToolCall>,
        context: ToolInvocationContext,
    ) -> HostFuture<Vec<JsObject>> {
        let state = self.state.clone();
        Box::pin(async move {
            let runtime = context.runtime.downcast::<DomainState>().unwrap();
            let execution = context.execution.downcast::<DomainState>().unwrap();
            assert!(Arc::ptr_eq(&state, &runtime));
            assert!(Arc::ptr_eq(&runtime, &execution));
            execution
                .accesses
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            runtime.signal.notify_one();
            Ok(vec![obj([("done", JsValue::Bool(true))])])
        })
    }
}
impl CapabilityResultObserver for DomainPort {
    fn on_capability_results(&self, _results: Vec<JsObject>) -> HostFuture<()> {
        let state = self.state.clone();
        Box::pin(async move {
            state.signal.notified().await;
            Ok(())
        })
    }
}
impl PermissionContext for DomainPort {
    fn prepare(&self, _value: JsValue) -> HostFuture<RuntimeContext> {
        let state = self.state.clone();
        Box::pin(async move { Ok(state as RuntimeContext) })
    }
}
impl PermissionDecision for DomainPort {
    fn decide(
        &self,
        _tool: Arc<dyn ToolDefinition>,
        _input: JsValue,
        context: RuntimeContext,
        _tool_call_id: JsString,
    ) -> HostFuture<JsValue> {
        let state = self.state.clone();
        Box::pin(async move {
            let context = context.downcast::<DomainState>().unwrap();
            assert!(Arc::ptr_eq(&state, &context));
            assert_eq!(
                context
                    .accesses
                    .fetch_add(1, std::sync::atomic::Ordering::SeqCst),
                1
            );
            Ok("allow".into())
        })
    }
}
#[tokio::test]
async fn host_context_retains_shared_state_and_service_identity() {
    let state = Arc::new(DomainState {
        signal: Notify::new(),
        accesses: std::sync::atomic::AtomicUsize::new(0),
    });
    let port = Arc::new(DomainPort {
        state: state.clone(),
        catalog: Fake::new(val([])),
    });
    let router = ModuleRouter {
        capability: capability::Handler::new(port.clone(), port.clone(), port.clone()),
        permission: permission::Handler::new(port.clone(), port.clone(), port),
        ..ModuleRouter::default()
    };
    tokio::time::timeout(std::time::Duration::from_secs(2), router.module_call(parse_json(r#"{"module":"capability","payload":{"operation":"execute","toolCallId":"id","name":"read"}}"#).unwrap())).await.unwrap().unwrap();
    router.module_call(parse_json(r#"{"module":"permission","payload":{"operation":"decide","tool":{"name":"read"},"toolCallId":"id"}}"#).unwrap()).await.unwrap();
    assert_eq!(state.accesses.load(std::sync::atomic::Ordering::SeqCst), 2);
}
