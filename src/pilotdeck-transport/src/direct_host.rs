//! Optional in-process host dispatch for embedded agent turns.
//!
//! When a [`super::stdio::ConnectionOwner`] carries a [`DirectHostCallback`],
//! module host requests are satisfied synchronously in the owning process
//! instead of being queued for a stdio peer.
use pilotdeck_agent::ports::{CallControl, PortResult};
use pilotdeck_protocol::{JsObject, JsString, JsValue};
use std::sync::Arc;

/// Gateway-side handler for a staged module host request line.
pub trait DirectHostCallback: Send + Sync {
    fn dispatch_module_call(
        &self,
        control: &CallControl,
        request: &JsValue,
    ) -> PortResult<JsObject>;
}

pub(crate) fn try_direct_dispatch(
    callback: &Option<Arc<dyn DirectHostCallback>>,
    control: &CallControl,
    request_line: &str,
) -> Option<PortResult<JsObject>> {
    let callback = callback.as_ref()?;
    let request = pilotdeck_protocol::parse_json(request_line).ok()?;
    Some(callback.dispatch_module_call(control, &request))
}

pub(crate) fn direct_module_response(
    request: &JsValue,
    result: Result<JsObject, pilotdeck_agent::ports::PortError>,
) -> JsValue {
    let mut response = JsObject::new();
    response.insert("kind".into(), "response".into());
    let id = request
        .get("messageId")
        .and_then(JsValue::as_string)
        .cloned()
        .unwrap_or_default();
    let mut message_id: Vec<u16> = "host-module-".encode_utf16().collect();
    message_id.extend_from_slice(id.units());
    response.insert(
        "messageId".into(),
        JsValue::String(JsString::from_utf16(message_id)),
    );
    response.insert("inReplyTo".into(), JsValue::String(id));
    if let Some(request_id) = request.get("requestId") {
        response.insert("requestId".into(), request_id.clone());
    }
    match result {
        Ok(payload) => {
            response.insert("ok".into(), JsValue::Bool(true));
            response.insert("payload".into(), JsValue::Object(payload));
        }
        Err(error) => {
            response.insert("ok".into(), JsValue::Bool(false));
            let message = match &error {
                pilotdeck_agent::ports::PortError::HostFailure { message, .. } => message.clone(),
                pilotdeck_agent::ports::PortError::InvalidResponse { message } => message.clone(),
                other => JsString::from(format!("{other:?}")),
            };
            response.insert(
                "error".into(),
                JsValue::Object({
                    let mut fields = JsObject::new();
                    fields.insert("message".into(), JsValue::String(message));
                    fields
                }),
            );
        }
    }
    JsValue::Object(response)
}
