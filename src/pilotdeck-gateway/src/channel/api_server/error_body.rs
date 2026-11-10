//! `createAgentStatusHttpErrorBody` for the api-server listener.
//!
//! Key order is the TS insertion order so the serialized body matches the
//! golden wire text: `error` then `agent_status`, and inside each object the
//! same field order as `agentStatus.ts`. #1615: the construction itself is
//! the shared [`crate::channel::agent_status`] port (the webhook lane
//! consumes the same one); this adapter fixes the api-server inputs — a
/// concrete `code`/`status` and the lane's `source`.
use crate::channel::agent_status::{self, HttpErrorBodyInput};
use pilotdeck_protocol::JsValue;

pub(crate) struct ErrorBody<'a> {
    pub event: &'a str,
    pub message: &'a str,
    pub code: &'a str,
    pub status: u16,
    pub kind: Option<&'a str>,
    pub user_hint: Option<&'a str>,
    pub scope: &'static str,
}

pub(crate) fn body(input: ErrorBody<'_>) -> JsValue {
    agent_status::create_agent_status_http_error_body(HttpErrorBodyInput {
        event: input.event.into(),
        event_name: input.event.into(),
        message: input.message.into(),
        code: Some(JsValue::String(input.code.into())),
        status: Some(input.status),
        r#type: input.kind.map(JsValue::from),
        user_hint: input.user_hint.map(JsValue::from),
        scope: input.scope,
        source: "api_server",
        detail: None,
    })
}
