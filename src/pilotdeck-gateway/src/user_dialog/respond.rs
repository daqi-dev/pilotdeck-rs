use super::*;
use crate::runtime::connection::RequestError;

pub(super) fn invalid(message: impl Into<JsString>) -> RequestError {
    let mut error = RequestError::new(message);
    error.code = "INVALID_USER_DIALOG_RESPONSE".into();
    error
}

pub(super) fn contract_error(request: &JsValue, recovered: bool) -> RequestError {
    let kind = request
        .get("dialogKind")
        .and_then(JsValue::as_string)
        .cloned()
        .unwrap_or_else(|| "undefined".into());
    let mut units = JsString::from("answered ").units().to_vec();
    units.extend_from_slice(kind.units());
    units.extend_from_slice(
        JsString::from(if recovered {
            " dialog result does not match the recovered dialog contract."
        } else {
            " dialog result does not match the pending dialog contract."
        })
        .units(),
    );
    invalid(JsString::from_utf16(units))
}

pub(super) fn accepts(request: &JsValue, value: &JsValue) -> bool {
    match request.get("dialogKind").and_then(JsValue::as_string) {
        Some(kind) if kind.equals_str("input") => value.as_string().is_some(),
        Some(kind) if kind.equals_str("select") => {
            value.as_string().is_some()
                && matches!(request.get("choices"), Some(JsValue::Array(choices)) if choices.iter().any(|choice| choice.get("value") == Some(value)))
        }
        Some(kind) if kind.equals_str("confirm") => value.as_bool().is_some(),
        _ => request
            .get("schema")
            .is_some_and(|schema| super::schema::accepts(schema, value)),
    }
}

pub(super) async fn respond(
    params: &JsValue,
    state: &GatewayUserDialogs,
) -> std::result::Result<JsValue, RequestError> {
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
            Some(JsValue::String(value)) if !super::routes::trim(value).is_empty() => Ok(value),
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
    let undefined = JsValue::Undefined;
    let result = params.get("result").unwrap_or(&undefined);
    let behavior = result.get("behavior").and_then(JsValue::as_string);
    let answer = match behavior {
        Some(behavior) if behavior.equals_str("answered") => {
            let value = result
                .get("value")
                .filter(|value| !value.is_undefined())
                .ok_or_else(|| invalid("answered user dialog results require a value."))?;
            DialogAnswer::Answered(value.clone())
        }
        Some(behavior) if behavior.equals_str("cancelled") => {
            let reason = match result.get("reason") {
                None | Some(JsValue::Undefined) => None,
                Some(JsValue::String(reason)) => Some(reason.clone()),
                _ => return Err(invalid("cancelled user dialog reason must be a string.")),
            };
            DialogAnswer::Cancelled(reason)
        }
        _ => return Err(invalid("result.behavior must be answered or cancelled.")),
    };
    let lease = match params.get("leaseId") {
        None | Some(JsValue::Undefined) => None,
        Some(JsValue::String(lease)) if !super::routes::trim(lease).is_empty() => Some(lease),
        _ => return Err(invalid("leaseId must be a non-empty string when provided.")),
    };
    if let Some(host) = &state.persistence
        && host.submit(params, request, lease, &answer).await?
    {
        return Ok(object([("delivered", JsValue::Bool(true))]));
    }
    let lease = lease.map(super::routes::trim);
    let has_pending = {
        let mut bus = state.bus.lock().unwrap();
        if let Some(entry) = bus.peek(session, request)
            && let DialogAnswer::Answered(value) = &answer
            && !(entry.accepts)(value)
        {
            return Err(contract_error(&entry.event, false));
        }
        if bus.response_requires_lease(session, request, lease.as_ref()) {
            return Err(lease_required(request));
        }
        bus.check_response_persistence(session, request)
            .map_err(|error| RequestError::new(error.to_string()))?;
        bus.has_pending(session, request)
    };
    // Persistence failure must leave the waiting tool reachable for retry.
    if has_pending && let Some(host) = &state.persistence {
        host.acknowledge(params, request).await?;
    }
    let consumed = state
        .bus
        .lock()
        .unwrap()
        .consume_for_response(
            session,
            request,
            lease.as_ref(),
            if matches!(answer, DialogAnswer::Answered(_)) {
                "answered"
            } else {
                "cancelled"
            },
        )
        .map_err(|error| RequestError::new(error.to_string()))?;
    match consumed {
        ConsumeResult::LeaseRequired => Err(lease_required(request)),
        ConsumeResult::Entry(entry) => {
            let answer = match answer {
                DialogAnswer::Cancelled(reason) => {
                    DialogAnswer::Cancelled(reason.filter(|reason| !reason.is_empty()))
                }
                answer => answer,
            };
            (entry.resolve)(answer);
            Ok(object([("delivered", JsValue::Bool(true))]))
        }
        ConsumeResult::NotPending => {
            if let Some(host) = &state.persistence {
                if host.recover(params, request, &answer).await? {
                    return Ok(object([
                        ("delivered", JsValue::Bool(true)),
                        ("recovered", JsValue::Bool(true)),
                        ("reason", "gateway_restarted".into()),
                    ]));
                }
                if host.acknowledge(params, request).await? {
                    return Ok(object([
                        ("delivered", JsValue::Bool(false)),
                        ("reason", "gateway_restarted".into()),
                    ]));
                }
            }
            Ok(object([("delivered", JsValue::Bool(false))]))
        }
    }
}

fn lease_required(request: &JsString) -> RequestError {
    let mut units = JsString::from("User dialog ").units().to_vec();
    units.extend_from_slice(request.units());
    units.extend_from_slice(JsString::from(" is currently claimed by another renderer.").units());
    let mut error = RequestError::new(JsString::from_utf16(units));
    error.code = "USER_DIALOG_LEASE_REQUIRED".into();
    error
}
