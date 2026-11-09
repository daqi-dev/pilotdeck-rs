use super::{GatewayInteractionCoordinator, object};
use crate::runtime::connection::RequestError;
use pilotdeck_protocol::JsValue;

pub fn handle(
    method: &str,
    params: &JsValue,
    interactions: &GatewayInteractionCoordinator,
) -> Option<Result<JsValue, RequestError>> {
    if method != "elicitation_respond" {
        return None;
    }
    Some(respond(params, interactions))
}

fn respond(
    params: &JsValue,
    interactions: &GatewayInteractionCoordinator,
) -> Result<JsValue, RequestError> {
    if matches!(params, JsValue::Undefined | JsValue::Null) {
        let kind = if params.is_undefined() {
            "undefined"
        } else {
            "null"
        };
        return Err(RequestError::new(format!(
            "Cannot read properties of {kind} (reading 'sessionKey')"
        )));
    }
    let delivered = match (
        params.get("sessionKey").and_then(JsValue::as_string),
        params.get("requestId").and_then(JsValue::as_string),
    ) {
        (Some(owner), Some(request)) => interactions.respond_elicitation_wire(
            owner,
            request,
            params.get("answer").cloned().unwrap_or(JsValue::Undefined),
            params.get("interactionBinding"),
        ),
        _ => false,
    };
    Ok(object([("delivered", JsValue::Bool(delivered))]))
}

#[cfg(test)]
#[path = "routes_tests.rs"]
mod tests;
