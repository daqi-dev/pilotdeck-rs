//! capability adapter extension point. Operations are implemented by the later A6 package.
use super::wire;
use crate::host::{HostFuture, JsObject, JsValue, ModuleError};

#[derive(Default)]
pub struct Handler;
impl Handler {
    pub fn call(&self, call: JsValue) -> HostFuture<JsObject> {
        Box::pin(async move {
            let payload = call.get("payload").and_then(JsValue::as_object);
            let operation = wire::string_field(payload, "operation")?;
            // No tools or plan/todo port are installed until the A6 adapter.
            if operation.equals_str("list_tools") {
                let mut response = JsObject::new();
                response.insert("tools".into(), JsValue::Array(Vec::new()));
                return Ok(response);
            }
            if operation.equals_str("plan_todo") {
                return Err(
                    ModuleError::new("Host did not provide a plan/todo capability.").into(),
                );
            }
            if operation.equals_str("execute") {
                wire::string_field(payload, "toolCallId")?;
                wire::string_field(payload, "name")?;
            }
            if operation.equals_str("execute_batch") {
                let Some(JsValue::Array(calls)) = payload.and_then(|p| p.get("calls")) else {
                    return Err(
                        ModuleError::new("Capability batch call must contain calls.").into(),
                    );
                };
                for call in calls {
                    wire::string_field(call.as_object(), "toolCallId")?;
                    wire::string_field(call.as_object(), "name")?;
                }
            }
            Err(wire::named_error(
                "Unsupported sidecar capability operation: ",
                operation,
                "",
            ))
        })
    }
    pub fn dispose(&self) -> HostFuture<()> {
        Box::pin(async { Ok(()) })
    }
}
