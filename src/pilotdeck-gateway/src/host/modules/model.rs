//! model adapter extension point. Operations are implemented by the later A6 package.
use super::wire;
use crate::host::{HostFuture, JsObject, JsValue};

#[derive(Default)]
pub struct Handler;
impl Handler {
    pub fn call(&self, call: JsValue) -> HostFuture<JsObject> {
        Box::pin(async move {
            let payload = call.get("payload").and_then(JsValue::as_object);
            let operation = wire::string_field(payload, "operation")?;
            // TS validates preparationId before its unsupported-operation arm.
            if !operation.equals_str("get_metadata") {
                wire::string_field(payload, "preparationId")?;
            }
            Err(wire::named_error(
                "Unsupported sidecar model operation: ",
                operation,
                "",
            ))
        })
    }
    pub fn dispose(&self) -> HostFuture<()> {
        Box::pin(async { Ok(()) })
    }
}
