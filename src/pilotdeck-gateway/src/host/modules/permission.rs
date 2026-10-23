//! Permission dispatch resolves trusted definitions from the catalog by name.
use super::{
    capability::{RuntimeContext, ToolCatalog, ToolDefinition},
    wire,
};
use crate::host::{HostFuture, JsObject, JsValue, ModuleError};
use pilotdeck_protocol::JsString;
use std::sync::Arc;

pub trait PermissionContext: Send + Sync {
    fn prepare(&self, value: JsValue) -> HostFuture<RuntimeContext>;
}
pub trait PermissionDecision: Send + Sync {
    fn decide(
        &self,
        tool: Arc<dyn ToolDefinition>,
        input: JsValue,
        context: RuntimeContext,
        tool_call_id: JsString,
    ) -> HostFuture<JsValue>;
}

#[derive(Clone)]
struct Ports {
    catalog: Arc<dyn ToolCatalog>,
    decision: Arc<dyn PermissionDecision>,
    context: Arc<dyn PermissionContext>,
}
#[derive(Clone, Default)]
pub struct Handler {
    ports: Option<Ports>,
}
impl Handler {
    pub fn new(
        catalog: Arc<dyn ToolCatalog>,
        decision: Arc<dyn PermissionDecision>,
        context: Arc<dyn PermissionContext>,
    ) -> Self {
        Self {
            ports: Some(Ports {
                catalog,
                decision,
                context,
            }),
        }
    }
    /// Whether composition installed a permission module. TS registers the
    /// handler only for an installed module (`dispatcher:181-189`).
    pub fn is_installed(&self) -> bool {
        self.ports.is_some()
    }
    pub fn call(&self, call: JsValue) -> HostFuture<JsObject> {
        let ports = self.ports.clone();
        Box::pin(async move {
            // An uninstalled module answers with the protocol's
            // unregistered-module error before any payload parsing, like the
            // TS client's handler lookup (`agentLoopSidecarClient.ts:1155-1158`).
            let Some(ports) = ports else {
                return Err(ModuleError::new("Unsupported sidecar host module: permission").into());
            };
            let payload = call.get("payload").and_then(JsValue::as_object);
            let operation = wire::string_field(payload, "operation")?;
            if !operation.equals_str("decide") {
                return Err(ModuleError::new("Unsupported sidecar permission operation.").into());
            }
            let tool_name = wire::string_field(
                payload
                    .and_then(|p| p.get("tool"))
                    .and_then(JsValue::as_object),
                "name",
            )?;
            let tool = ports
                .catalog
                .list()
                .await?
                .into_iter()
                .find(|tool| {
                    tool.fields().get("name").and_then(JsValue::as_string) == Some(tool_name)
                })
                .ok_or_else(|| {
                    wire::named_error(
                        "Permission request references unavailable tool: ",
                        tool_name,
                        "",
                    )
                })?;
            let context = ports
                .context
                .prepare(
                    payload
                        .and_then(|p| p.get("context"))
                        .cloned()
                        .unwrap_or(JsValue::Undefined),
                )
                .await?;
            let tool_call_id = wire::string_field(payload, "toolCallId")?.clone();
            let input = payload
                .and_then(|p| p.get("input"))
                .cloned()
                .unwrap_or(JsValue::Undefined);
            let decision = ports
                .decision
                .decide(tool, input, context, tool_call_id)
                .await?;
            let mut response = JsObject::new();
            response.insert("decision".into(), decision);
            Ok(response)
        })
    }
    pub fn dispose(&self) -> HostFuture<()> {
        Box::pin(async { Ok(()) })
    }
}
