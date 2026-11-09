use super::*;
use crate::runtime::connection::RequestError;

pub async fn handle(
    method: &str,
    params: &JsValue,
    state: &GatewayUserDialogs,
) -> Option<std::result::Result<JsValue, RequestError>> {
    match method {
        "user_dialog_list" => Some(list(params, state).await),
        "user_dialog_claim" => Some(claim(params, state).await),
        "user_dialog_release" => Some(release(params, state).await),
        "user_dialog_respond" => Some(super::respond::respond(params, state).await),
        _ => None,
    }
}

async fn release(
    params: &JsValue,
    state: &GatewayUserDialogs,
) -> std::result::Result<JsValue, RequestError> {
    for name in ["sessionKey", "requestId", "leaseId"] {
        if matches!(params, JsValue::Undefined | JsValue::Null) {
            let kind = if params.is_undefined() {
                "undefined"
            } else {
                "null"
            };
            return Err(RequestError::new(format!(
                "Cannot read properties of {kind} (reading '{name}')"
            )));
        }
        match params.get(name) {
            Some(JsValue::String(value)) if !trim(value).is_empty() => {}
            None | Some(JsValue::Undefined | JsValue::Null | JsValue::String(_)) => {
                let mut error =
                    RequestError::new("sessionKey, requestId, and leaseId are required.");
                error.code = "INVALID_USER_DIALOG_RELEASE".into();
                return Err(error);
            }
            _ => {
                return Err(RequestError::new(format!(
                    "input.{name}?.trim is not a function"
                )));
            }
        }
    }
    let session = params.get("sessionKey").unwrap().as_string().unwrap();
    let request = params.get("requestId").unwrap().as_string().unwrap();
    let lease = params.get("leaseId").unwrap().as_string().unwrap();
    let local = state
        .bus
        .lock()
        .unwrap()
        .release(session, request, &trim(lease));
    let released = if local {
        true
    } else if let Some(host) = &state.persistence {
        host.release(params, request, lease).await?
    } else {
        false
    };
    Ok(object([("released", JsValue::Bool(released))]))
}

pub(super) fn trim(value: &JsString) -> JsString {
    let space =
        |unit: &u16| char::from_u32(u32::from(*unit)).is_some_and(crate::js_value::js_space);
    let units = value.units();
    let start = units
        .iter()
        .position(|unit| !space(unit))
        .unwrap_or(units.len());
    let end = units
        .iter()
        .rposition(|unit| !space(unit))
        .map_or(start, |index| index + 1);
    JsString::from_utf16(units[start..end].to_vec())
}

async fn claim(
    params: &JsValue,
    state: &GatewayUserDialogs,
) -> std::result::Result<JsValue, RequestError> {
    let invalid = |message| {
        let mut error = RequestError::new(message);
        error.code = "INVALID_USER_DIALOG_CLAIM".into();
        error
    };
    let required = |name: &str| -> std::result::Result<&JsString, RequestError> {
        if matches!(params, JsValue::Undefined | JsValue::Null) {
            let kind = if params.is_undefined() {
                "undefined"
            } else {
                "null"
            };
            return Err(RequestError::new(format!(
                "Cannot read properties of {kind} (reading '{name}')"
            )));
        }
        match params.get(name) {
            Some(JsValue::String(value)) if !trim(value).is_empty() => Ok(value),
            None | Some(JsValue::Undefined | JsValue::Null | JsValue::String(_)) => {
                Err(invalid("sessionKey and requestId are required."))
            }
            _ => Err(RequestError::new(format!(
                "input.{name}?.trim is not a function"
            ))),
        }
    };
    let session = required("sessionKey")?;
    let request = required("requestId")?;
    let lease = match params.get("leaseId") {
        None | Some(JsValue::Undefined) => None,
        Some(JsValue::String(lease)) if !trim(lease).is_empty() => Some(lease),
        _ => return Err(invalid("leaseId must be a non-empty string when provided.")),
    };
    let ttl = match params.get("ttlMs") {
        None | Some(JsValue::Undefined | JsValue::Null) => Some(30_000.0),
        Some(value) => value.as_number(),
    };
    let ttl = ttl
        .filter(|ttl| ttl.fract() == 0.0 && (1000.0..=300_000.0).contains(ttl))
        .ok_or_else(|| invalid("ttlMs must be a safe integer between 1000 and 300000."))?;
    let hosted = if let Some(host) = &state.persistence {
        let hosted = host.claim(params, request, ttl, lease).await?;
        if hosted.get("reason") != Some(&"not_pending".into()) {
            return Ok(hosted);
        }
        Some(hosted)
    } else {
        None
    };
    let lease = lease.map(trim);
    let claim = state
        .bus
        .lock()
        .unwrap()
        .claim(session, request, ttl, lease.as_ref())
        .map_err(|error| RequestError::new(error.to_string()))?;
    Ok(if matches!(claim, LeaseClaim::NotPending) {
        hosted.unwrap_or_else(|| super::http_store::lease_claim_value(&claim))
    } else {
        super::http_store::lease_claim_value(&claim)
    })
}
async fn list(
    params: &JsValue,
    state: &GatewayUserDialogs,
) -> std::result::Result<JsValue, RequestError> {
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
    let undefined = JsValue::Undefined;
    let session = match params.get("sessionKey").unwrap_or(&undefined) {
        JsValue::String(session)
            if session.units().iter().any(|unit| {
                !char::from_u32(u32::from(*unit)).is_some_and(crate::js_value::js_space)
            }) =>
        {
            session
        }
        JsValue::Undefined | JsValue::Null | JsValue::String(_) => {
            let mut error = RequestError::new("sessionKey is required.");
            error.code = "INVALID_USER_DIALOG_LIST".into();
            return Err(error);
        }
        _ => {
            return Err(RequestError::new(
                "input.sessionKey?.trim is not a function",
            ));
        }
    };
    let live = state.bus.lock().unwrap().list(session);
    let hosted = if let Some(host) = &state.persistence {
        host.hosted(params).await?
    } else {
        Vec::new()
    };
    let recovered = if let Some(host) = &state.persistence {
        host.recovered(params).await?
    } else {
        Vec::new()
    };
    let id = |event: &JsValue| {
        event
            .get("requestId")
            .cloned()
            .unwrap_or(JsValue::Undefined)
    };
    let live_ids: Vec<_> = live.iter().map(id).collect();
    let hosted_ids: Vec<_> = hosted.iter().map(id).collect();
    let mut dialogs: Vec<_> = live
        .into_iter()
        .filter(|event| !hosted_ids.contains(&id(event)))
        .collect();
    dialogs.extend(hosted);
    dialogs.extend(recovered.into_iter().filter(|event| {
        let id = event
            .get("request")
            .and_then(|request| request.get("requestId"))
            .cloned()
            .unwrap_or(JsValue::Undefined);
        !live_ids.contains(&id) && !hosted_ids.contains(&id)
    }));
    Ok(object([("dialogs", JsValue::Array(dialogs))]))
}

#[cfg(test)]
#[path = "routes_tests.rs"]
mod tests;
