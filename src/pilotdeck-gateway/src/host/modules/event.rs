//! event adapter extension point. Operations are implemented by the later A6 package.
use super::wire;
use crate::host::{HostFuture, JsObject, JsValue, ModuleError};

#[derive(Default)]
pub struct Handler;
impl Handler {
    pub fn call(&self, call: JsValue) -> HostFuture<JsObject> {
        Box::pin(async move {
            let payload = call.get("payload").and_then(JsValue::as_object);
            let _operation = wire::string_field(payload, "operation")?;
            Err(ModuleError::new("Unsupported sidecar event operation.").into())
        })
    }
    pub fn dispose(&self) -> HostFuture<()> {
        Box::pin(async { Ok(()) })
    }
}
