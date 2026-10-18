//! model adapter extension point. Operations are implemented by the later A6 package.
use super::wire;
use crate::host::{HostFuture, JsObject, JsValue, ModuleError};

#[derive(Default)]
pub struct Handler;
impl Handler {
    pub fn call(&self, call: JsValue) -> HostFuture<JsObject> {
        Box::pin(async move {
            let payload = call.get("payload").and_then(JsValue::as_object);
            let operation = wire::string_field(payload, "operation")?;
            // Match the fixed TS dispatcher's empty-port/empty-turn boundary.
            // Actual model invocation and metadata ports belong to A6.4.
            if operation.equals_str("get_metadata") {
                return Err(
                    ModuleError::new("Host model metadata capability is unavailable.").into(),
                );
            }
            let preparation = wire::string_field(payload, "preparationId")?;
            if operation.equals_str("prepare") {
                let request = payload
                    .and_then(|p| p.get("request"))
                    .and_then(JsValue::as_object);
                if !request.is_some_and(|r| {
                    matches!(r.get("provider"), Some(JsValue::String(_)))
                        && matches!(r.get("model"), Some(JsValue::String(_)))
                        && matches!(r.get("messages"), Some(JsValue::Array(_)))
                }) {
                    return Err(ModuleError::new(
                        "Sidecar model call contains an invalid canonical request.",
                    )
                    .into());
                }
            }
            if ["materialize_prepared_request", "stream", "stream_next"]
                .iter()
                .any(|name| operation.equals_str(name))
            {
                return Err(wire::named_error(
                    "Unknown sidecar model preparation: ",
                    preparation,
                    "",
                ));
            }
            if operation.equals_str("close_stream") {
                let mut response = JsObject::new();
                response.insert("closed".into(), JsValue::Bool(true));
                return Ok(response);
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
