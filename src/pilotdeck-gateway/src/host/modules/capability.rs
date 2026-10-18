//! Tool dispatch at the host boundary (TS sidecarDefaultModuleDispatcher:153-179).
use super::wire;
use crate::host::{HostError, HostFuture, JsObject, JsValue, ModuleError};
use pilotdeck_protocol::JsString;
use std::{any::Any, sync::Arc};

/// Catalog metadata and pure, nonblocking descriptor predicates. A failed
/// predicate is represented by Err and serialized as false, like TS safely().
pub trait ToolDefinition: Send + Sync {
    fn fields(&self) -> &JsObject;
    fn is_read_only(&self, _input: &JsValue) -> Result<JsValue, HostError> {
        Ok(JsValue::Bool(false))
    }
    fn is_concurrency_safe(&self, _input: &JsValue) -> Result<JsValue, HostError> {
        Ok(JsValue::Bool(false))
    }
    fn requires_user_interaction(&self, _input: &JsValue) -> Result<JsValue, HostError> {
        Ok(JsValue::Bool(false))
    }
    fn required_runtime_capabilities(&self) -> Option<Vec<JsString>> {
        None
    }
}

pub trait ToolCatalog: Send + Sync {
    fn list(&self) -> HostFuture<Vec<Arc<dyn ToolDefinition>>>;
}

#[derive(Clone, Debug, PartialEq)]
pub struct ToolCall {
    pub id: JsString,
    pub name: JsString,
    pub input: JsValue,
}

/// Context construction is turn-bound; checkpoint/plan state is supplied by
/// the later runtime adapter, never reconstructed from sidecar authority here.
pub struct ToolInvocationContext {
    pub runtime: RuntimeContext,
    pub execution: RuntimeContext,
}
/// Host-owned context can retain cancellation, checkpoint state and service
/// handles. It is deliberately opaque to the wire adapter, not JSON data.
pub type RuntimeContext = Arc<dyn Any + Send + Sync>;
pub trait ToolContext: Send + Sync {
    fn prepare(&self, call: JsValue) -> HostFuture<ToolInvocationContext>;
}

/// Port implementations must return promptly and do I/O asynchronously.
pub trait ToolExecution: ToolCatalog {
    fn refresh(&self) -> HostFuture<()> {
        Box::pin(async { Ok(()) })
    }
    fn execute_all(
        &self,
        calls: Vec<ToolCall>,
        context: ToolInvocationContext,
    ) -> HostFuture<Vec<JsObject>>;
}
pub trait CapabilityResultObserver: Send + Sync {
    fn on_capability_results(&self, results: Vec<JsObject>) -> HostFuture<()>;
}
pub trait PlanTodoHandler: Send + Sync {
    fn call(&self, call: JsValue) -> HostFuture<JsObject>;
}

#[derive(Clone)]
struct Ports {
    execution: Arc<dyn ToolExecution>,
    context: Arc<dyn ToolContext>,
    observer: Arc<dyn CapabilityResultObserver>,
}
#[derive(Clone, Default)]
pub struct Handler {
    ports: Option<Ports>,
    plan_todo: Option<Arc<dyn PlanTodoHandler>>,
}
impl Handler {
    pub fn new(
        execution: Arc<dyn ToolExecution>,
        context: Arc<dyn ToolContext>,
        observer: Arc<dyn CapabilityResultObserver>,
    ) -> Self {
        Self {
            ports: Some(Ports {
                execution,
                context,
                observer,
            }),
            plan_todo: None,
        }
    }
    pub fn with_plan_todo(mut self, handler: Arc<dyn PlanTodoHandler>) -> Self {
        self.plan_todo = Some(handler);
        self
    }
    pub fn call(&self, call: JsValue) -> HostFuture<JsObject> {
        let handler = self.clone();
        Box::pin(async move {
            let payload = call.get("payload").and_then(JsValue::as_object);
            let operation = wire::string_field(payload, "operation")?;
            if operation.equals_str("plan_todo") {
                return match handler.plan_todo {
                    Some(plan) => plan.call(call).await,
                    None => {
                        Err(ModuleError::new("Host did not provide a plan/todo capability.").into())
                    }
                };
            }
            if operation.equals_str("list_tools") {
                let mut tools = Vec::new();
                if let Some(ports) = &handler.ports {
                    ports.execution.refresh().await?;
                    tools = ports
                        .execution
                        .list()
                        .await?
                        .iter()
                        .map(|tool| JsValue::Object(serialize_tool_descriptor(tool.as_ref())))
                        .collect();
                }
                let mut response = JsObject::new();
                response.insert("tools".into(), JsValue::Array(tools));
                return Ok(response);
            }

            // TS builds context before validating calls, even for unknown ops.
            let context = match &handler.ports {
                Some(ports) => Some(ports.context.prepare(call.clone()).await?),
                None => None,
            };
            let calls = if operation.equals_str("execute_batch") {
                let Some(JsValue::Array(calls)) = payload.and_then(|p| p.get("calls")) else {
                    return Err(
                        ModuleError::new("Capability batch call must contain calls.").into(),
                    );
                };
                Some(
                    calls
                        .iter()
                        .map(parse_tool_call)
                        .collect::<Result<Vec<_>, _>>()?,
                )
            } else if operation.equals_str("execute") {
                Some(vec![parse_tool_call(call.get("payload").unwrap())?])
            } else {
                None
            };
            let (Some(calls), Some(ports), Some(context)) = (calls, handler.ports, context) else {
                return Err(wire::named_error(
                    "Unsupported sidecar capability operation: ",
                    operation,
                    "",
                ));
            };
            let count = calls.len();
            let results = ports.execution.execute_all(calls, context).await?;
            if operation.equals_str("execute_batch") {
                if results.len() != count {
                    return Err(ModuleError::new(
                        "Capability port returned an incomplete batch result.",
                    )
                    .into());
                }
                ports
                    .observer
                    .on_capability_results(results.clone())
                    .await?;
                let mut response = JsObject::new();
                response.insert(
                    "results".into(),
                    JsValue::Array(results.into_iter().map(JsValue::Object).collect()),
                );
                Ok(response)
            } else {
                let result = results
                    .into_iter()
                    .next()
                    .ok_or_else(|| ModuleError::new("Capability port returned no result."))?;
                ports
                    .observer
                    .on_capability_results(vec![result.clone()])
                    .await?;
                Ok(result)
            }
        })
    }
    pub fn dispose(&self) -> HostFuture<()> {
        // These borrowed domain ports are not owned resources of the dispatcher.
        Box::pin(async { Ok(()) })
    }
}

fn parse_tool_call(value: &JsValue) -> Result<ToolCall, HostError> {
    let fields = value.as_object();
    Ok(ToolCall {
        id: wire::string_field(fields, "toolCallId")?.clone(),
        name: wire::string_field(fields, "name")?.clone(),
        input: match value.get("arguments") {
            None | Some(JsValue::Undefined | JsValue::Null) => JsValue::Object(JsObject::new()),
            Some(value) => value.clone(),
        },
    })
}

pub fn serialize_tool_descriptor(tool: &dyn ToolDefinition) -> JsObject {
    let mut descriptor = JsObject::new();
    for field in ["name", "description", "kind", "inputSchema"] {
        if let Some(value) = tool.fields().get(field) {
            descriptor.insert(field.into(), value.clone());
        }
    }
    let empty = JsValue::Object(JsObject::new());
    descriptor.insert(
        "readOnly".into(),
        tool.is_read_only(&empty).unwrap_or(JsValue::Bool(false)),
    );
    descriptor.insert(
        "concurrencySafe".into(),
        tool.is_concurrency_safe(&empty)
            .unwrap_or(JsValue::Bool(false)),
    );
    let interaction = tool
        .requires_user_interaction(&empty)
        .unwrap_or(JsValue::Bool(false));
    descriptor.insert(
        "requiresUserInteraction".into(),
        if matches!(interaction, JsValue::Undefined | JsValue::Null) {
            JsValue::Bool(false)
        } else {
            interaction
        },
    );
    if let Some(capabilities) = tool.required_runtime_capabilities() {
        descriptor.insert(
            "requiredRuntimeCapabilities".into(),
            JsValue::Array(capabilities.into_iter().map(JsValue::String).collect()),
        );
    }
    descriptor
}
