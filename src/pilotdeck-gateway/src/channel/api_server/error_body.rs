//! `createAgentStatusHttpErrorBody` for the api-server listener.
//!
//! Key order is the TS insertion order so the serialized body matches the
//! golden wire text: `error` then `agent_status`, and inside each object the
//! same field order as `agentStatus.ts`.
use crate::metadata::object;
use pilotdeck_protocol::JsValue;

pub(crate) struct ErrorBody<'a> {
    pub event: &'a str,
    pub message: &'a str,
    pub code: &'a str,
    pub status: u16,
    pub kind: Option<&'a str>,
    pub user_hint: Option<&'a str>,
    pub scope: &'a str,
}

pub(crate) fn body(input: ErrorBody<'_>) -> JsValue {
    let user_hint = input
        .user_hint
        .map(str::to_owned)
        .unwrap_or_else(|| default_user_hint(input.status));
    let kind = input
        .kind
        .map(str::to_owned)
        .unwrap_or_else(|| default_error_type(input.status));
    let mut detail = vec![
        ("message", JsValue::from(input.message)),
        ("code", JsValue::from(input.code)),
        ("severity", JsValue::from("error")),
        ("visible", JsValue::Bool(true)),
        ("userHint", JsValue::from(user_hint.as_str())),
        ("scope", JsValue::from(input.scope)),
        ("source", JsValue::from("api_server")),
        ("status", JsValue::Number(input.status as f64)),
    ];
    detail.shrink_to_fit();
    object([
        (
            "error",
            object([
                ("message", JsValue::from(input.message)),
                ("type", JsValue::from(kind.as_str())),
                ("code", JsValue::from(input.code)),
                ("userHint", JsValue::from(user_hint.as_str())),
                ("status", JsValue::Number(input.status as f64)),
                ("scope", JsValue::from(input.scope)),
                ("source", JsValue::from("api_server")),
                ("event", JsValue::from(input.event)),
            ]),
        ),
        (
            "agent_status",
            object([
                ("type", JsValue::from("agent_status")),
                ("event", JsValue::from(input.event)),
                ("detail", object(detail)),
            ]),
        ),
    ])
}

fn default_user_hint(status: u16) -> String {
    match status {
        401 | 403 => "Check authentication and permissions, then retry.".into(),
        429 => "Wait for the rate limit to reset, then retry.".into(),
        413 => "Reduce the request size and retry.".into(),
        status if status >= 500 => {
            "The server is unavailable or returned an internal error. Retry later or check server logs."
                .into()
        }
        _ => "Fix the request and retry.".into(),
    }
}

fn default_error_type(status: u16) -> String {
    match status {
        429 => "rate_limit_error".into(),
        status if status >= 500 => "server_error".into(),
        _ => "invalid_request_error".into(),
    }
}
