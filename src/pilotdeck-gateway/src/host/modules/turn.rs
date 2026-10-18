//! turn adapter extension point. Operations are implemented by the later A6 package.
use super::wire;
use crate::host::{HostFuture, JsObject, JsValue};

#[derive(Default)]
pub struct Handler;
impl Handler {
    pub fn call(&self, call: JsValue) -> HostFuture<JsObject> {
        Box::pin(async move {
            let payload = call.get("payload").and_then(JsValue::as_object);
            let operation = wire::string_field(payload, "operation")?;
            Err(wire::named_error(
                "Host turn capability does not support ",
                operation,
                ".",
            ))
        })
    }
    pub fn dispose(&self) -> HostFuture<()> {
        Box::pin(async { Ok(()) })
    }
}
