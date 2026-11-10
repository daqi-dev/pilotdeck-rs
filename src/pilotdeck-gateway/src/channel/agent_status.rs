//! `src/status/agentStatus.ts` (baseline cc7c4e6), plus the
//! visible-failure predicates and event-name set its two consumers share:
//! `webhook-render.ts` and `api-server-render.ts` import the same
//! `agentStatus` module on the TS side, so the Rust port lives here once
//! (#1615) instead of one copy per lane. Key insertion order is the JS
//! object literal order, so the JSON bodies stringify identically; `source`
//! is the only field the lanes differ in, and it is an input.
use pilotdeck_protocol::{JsObject, JsString, JsValue};

use crate::runtime::memory::js_trim_units;

/// `createVisibleErrorStatusDetail`: severity defaults to `"error"`, visible
/// is always true, then the caller's `detail` spread.
pub(crate) fn create_visible_error_status_detail(input: StatusDetailInput) -> JsObject {
    let mut detail = JsObject::new();
    detail.insert("message".into(), JsValue::String(input.message));
    if let Some(message_i18n) = input.message_i18n {
        detail.insert("messageI18n".into(), message_i18n);
    }
    if let Some(code) = input.code {
        detail.insert("code".into(), code);
    }
    detail.insert("severity".into(), JsValue::String(input.severity.into()));
    detail.insert("visible".into(), JsValue::Bool(true));
    if let Some(user_hint) = input.user_hint {
        detail.insert("userHint".into(), user_hint);
    }
    if let Some(user_hint_i18n) = input.user_hint_i18n {
        detail.insert("userHintI18n".into(), user_hint_i18n);
    }
    detail.insert("scope".into(), JsValue::String(input.scope.into()));
    detail.insert("source".into(), JsValue::String(input.source.into()));
    if let Some(extra) = input.detail {
        for (name, value) in extra.own_entries() {
            detail.insert(name.clone(), value.clone());
        }
    }
    detail
}

/// `createAgentStatusHttpErrorBody`: `{error, agent_status}`. The lane's
/// `source` (`"webhook"` / `"api_server"`) is the only lane-specific byte.
pub(crate) fn create_agent_status_http_error_body(input: HttpErrorBodyInput) -> JsValue {
    let code = input
        .code
        .unwrap_or_else(|| JsValue::String(input.event.clone()));
    let user_hint = input
        .user_hint
        .unwrap_or_else(|| JsValue::String(default_user_hint_for_http_status(input.status).into()));
    let mut detail_extra = JsObject::new();
    if let Some(status) = input.status {
        detail_extra.insert("status".into(), JsValue::Number(f64::from(status)));
    }
    if let Some(extra) = input.detail {
        for (name, value) in extra.own_entries() {
            detail_extra.insert(name.clone(), value.clone());
        }
    }
    let detail = create_visible_error_status_detail(StatusDetailInput {
        message: input.message.clone(),
        message_i18n: None,
        code: Some(code.clone()),
        severity: "error",
        user_hint: Some(user_hint.clone()),
        user_hint_i18n: None,
        scope: input.scope,
        source: input.source,
        detail: Some(detail_extra),
    });
    let mut error = JsObject::new();
    error.insert("message".into(), JsValue::String(input.message));
    error.insert(
        "type".into(),
        input
            .r#type
            .unwrap_or_else(|| JsValue::String(default_http_error_type(input.status).into())),
    );
    error.insert("code".into(), code);
    error.insert("userHint".into(), user_hint);
    if let Some(status) = input.status {
        error.insert("status".into(), JsValue::Number(f64::from(status)));
    }
    error.insert("scope".into(), JsValue::String(input.scope.into()));
    error.insert("source".into(), JsValue::String(input.source.into()));
    error.insert("event".into(), JsValue::String(input.event));
    let mut agent_status = JsObject::new();
    agent_status.insert("type".into(), JsValue::String("agent_status".into()));
    agent_status.insert("event".into(), JsValue::String(input.event_name));
    agent_status.insert("detail".into(), JsValue::Object(detail));
    let mut body = JsObject::new();
    body.insert("error".into(), JsValue::Object(error));
    body.insert("agent_status".into(), JsValue::Object(agent_status));
    JsValue::Object(body)
}

/// `isVisibleFailureStatusDetail`: a record with `visible !== false` and
/// `severity === "error"`.
pub(crate) fn is_visible_failure_status_detail(detail: Option<&JsValue>) -> bool {
    let Some(object) = detail.and_then(JsValue::as_object) else {
        return false;
    };
    object
        .get("visible")
        .is_none_or(|value| *value != JsValue::Bool(false))
        && object.get("severity") == Some(&JsValue::String("error".into()))
}

/// `VISIBLE_FAILURE_STATUS_EVENTS`, the `agent_status.event` names that
/// render a visible failure even without a detail record. One copy for both
/// renderers (#1615); the api-server negative control mutates this table.
const VISIBLE_FAILURE_STATUS_EVENTS: [&str; 17] = [
    "model_empty_response_exhausted",
    "max_turns_reached",
    "max_output_recovery_exhausted",
    "model_request_failed",
    "tool_call_recovery_exhausted",
    "tool_error_loop",
    "lifecycle_blocked",
    "turn_failed",
    "turn_timeout",
    "gateway_submit_failed",
    "session_busy",
    "gateway_bridge_error",
    "gateway_stream_ended_without_completion",
    "channel_submit_failed",
    "subagent_failed",
    "content_filter_stop",
    "unknown_finish_reason",
];

pub(crate) fn is_visible_failure_event_name(name: &str) -> bool {
    VISIBLE_FAILURE_STATUS_EVENTS.contains(&name)
}

/// `isVisibleFailureAgentStatusEvent`: `detail?.visible !== false` and (the
/// event name is in the set or the detail is a visible failure status). Both
/// renderers' `agent_status` arm funnels through this predicate.
pub(crate) fn is_visible_failure_agent_status_event(event: &JsValue) -> bool {
    if matches!(
        event.get("detail").and_then(|detail| detail.get("visible")),
        Some(JsValue::Bool(false))
    ) {
        return false;
    }
    let in_set = event
        .get("event")
        .and_then(JsValue::as_string)
        .and_then(|name| name.to_utf8().ok())
        .is_some_and(|name| is_visible_failure_event_name(&name));
    in_set || is_visible_failure_status_detail(event.get("detail"))
}

/// The detail's non-blank string `message`, if any: the message a visible
/// failure renders. Blankness uses the JS trim (U+FEFF included).
pub(crate) fn status_message(detail: Option<&JsValue>) -> Option<JsString> {
    detail
        .and_then(JsValue::as_object)
        .and_then(|object| object.get("message"))
        .and_then(JsValue::as_string)
        .filter(|message| !js_trim_units(message.units()).is_empty())
        .cloned()
}

/// `visibleStatusMessage`: the detail's non-blank string message, else the
/// fallback, as a Rust `String` (UTF-16 lossy, the webhook renderer's
/// flavor; the api-server renderer keeps `JsString` and calls
/// [`status_message`] itself).
pub(crate) fn visible_status_message(detail: Option<&JsValue>, fallback: &str) -> String {
    status_message(detail)
        .map(|message| String::from_utf16_lossy(message.units()))
        .unwrap_or_else(|| fallback.to_owned())
}

fn default_user_hint_for_http_status(status: Option<u16>) -> &'static str {
    match status {
        Some(401 | 403) => "Check authentication and permissions, then retry.",
        Some(429) => "Wait for the rate limit to reset, then retry.",
        Some(413) => "Reduce the request size and retry.",
        Some(status) if status >= 500 => {
            "The server is unavailable or returned an internal error. Retry later or check server logs."
        }
        _ => "Fix the request and retry.",
    }
}

fn default_http_error_type(status: Option<u16>) -> &'static str {
    match status {
        Some(429) => "rate_limit_error",
        Some(status) if status >= 500 => "server_error",
        _ => "invalid_request_error",
    }
}

pub(crate) type Scope = &'static str;
pub(crate) type Severity = &'static str;
pub(crate) type Source = &'static str;

pub(crate) struct StatusDetailInput {
    pub message: JsString,
    pub message_i18n: Option<JsValue>,
    pub code: Option<JsValue>,
    pub severity: Severity,
    pub user_hint: Option<JsValue>,
    pub user_hint_i18n: Option<JsValue>,
    pub scope: Scope,
    pub source: Source,
    pub detail: Option<JsObject>,
}

pub(crate) struct HttpErrorBodyInput {
    pub event: JsString,
    pub message: JsString,
    pub code: Option<JsValue>,
    pub status: Option<u16>,
    pub r#type: Option<JsValue>,
    pub user_hint: Option<JsValue>,
    pub scope: Scope,
    pub source: Source,
    pub detail: Option<JsObject>,
    /// The `event` repeated inside `agent_status`; the error-shape variants
    /// of the TS call sites pass the same string in both positions.
    pub event_name: JsString,
}

#[cfg(test)]
mod tests {
    use super::*;
    use pilotdeck_protocol::stringify_json;

    fn body_text(value: &JsValue) -> String {
        stringify_json(value)
            .expect("#1334 body json")
            .expect("#1334 object")
    }

    #[test]
    fn error_body_matches_the_ts_key_order() {
        let body = create_agent_status_http_error_body(HttpErrorBodyInput {
            event: "webhook_rate_limited".into(),
            event_name: "webhook_rate_limited".into(),
            message: "Rate limit exceeded".into(),
            code: Some("rate_limit_exceeded".into()),
            status: Some(429),
            r#type: Some("rate_limit_error".into()),
            user_hint: Some("Wait for the webhook rate limit window to reset, then retry.".into()),
            scope: "http",
            source: "webhook",
            detail: None,
        });
        assert_eq!(
            body_text(&body),
            "{\"error\":{\"message\":\"Rate limit exceeded\",\"type\":\"rate_limit_error\",\
             \"code\":\"rate_limit_exceeded\",\"userHint\":\"Wait for the webhook rate limit \
             window to reset, then retry.\",\"status\":429,\"scope\":\"http\",\"source\":\
             \"webhook\",\"event\":\"webhook_rate_limited\"},\"agent_status\":{\"type\":\
             \"agent_status\",\"event\":\"webhook_rate_limited\",\"detail\":{\"message\":\
             \"Rate limit exceeded\",\"code\":\"rate_limit_exceeded\",\"severity\":\"error\",\
             \"visible\":true,\"userHint\":\"Wait for the webhook rate limit window to reset, \
             then retry.\",\"scope\":\"http\",\"source\":\"webhook\",\"status\":429}}}"
        );
    }

    /// #1615: the api-server lane's body is the same construction with its own
    /// `source`; only that field's bytes differ.
    #[test]
    fn error_body_key_order_is_source_independent() {
        let webhook = body_text(&create_agent_status_http_error_body(HttpErrorBodyInput {
            event: "api_error".into(),
            event_name: "api_error".into(),
            message: "Request failed".into(),
            code: Some("api_error".into()),
            status: Some(503),
            r#type: None,
            user_hint: None,
            scope: "http",
            source: "webhook",
            detail: None,
        }));
        let api_server = body_text(&create_agent_status_http_error_body(HttpErrorBodyInput {
            event: "api_error".into(),
            event_name: "api_error".into(),
            message: "Request failed".into(),
            code: Some("api_error".into()),
            status: Some(503),
            r#type: None,
            user_hint: None,
            scope: "http",
            source: "api_server",
            detail: None,
        }));
        assert_eq!(
            webhook.replace("\"webhook\"", "\"lane\""),
            api_server.replace("\"api_server\"", "\"lane\""),
            "#1615 the two lanes' error bodies differ only in the source bytes"
        );
        assert_eq!(
            api_server,
            "{\"error\":{\"message\":\"Request failed\",\"type\":\"server_error\",\"code\":\
             \"api_error\",\"userHint\":\"The server is unavailable or returned an internal \
             error. Retry later or check server logs.\",\"status\":503,\"scope\":\"http\",\
             \"source\":\"api_server\",\"event\":\"api_error\"},\"agent_status\":{\"type\":\
             \"agent_status\",\"event\":\"api_error\",\"detail\":{\"message\":\"Request \
             failed\",\"code\":\"api_error\",\"severity\":\"error\",\"visible\":true,\
             \"userHint\":\"The server is unavailable or returned an internal error. Retry \
             later or check server logs.\",\"scope\":\"http\",\"source\":\"api_server\",\
             \"status\":503}}}"
        );
    }

    #[test]
    fn defaults_fill_code_type_and_user_hint() {
        let body = create_agent_status_http_error_body(HttpErrorBodyInput {
            event: "webhook_unknown_route".into(),
            event_name: "webhook_unknown_route".into(),
            message: "Unknown route: nope".into(),
            code: None,
            status: Some(404),
            r#type: None,
            user_hint: None,
            scope: "http",
            source: "webhook",
            detail: None,
        });
        let text = body_text(&body);
        assert!(
            text.contains("\"code\":\"webhook_unknown_route\""),
            "#1334 code default"
        );
        assert!(
            text.contains("\"type\":\"invalid_request_error\""),
            "#1334 type default"
        );
        assert!(
            text.contains("\"userHint\":\"Fix the request and retry.\""),
            "#1334 hint default"
        );
    }

    #[test]
    fn visibility_requires_error_severity_and_not_hidden() {
        let mut detail = JsObject::new();
        detail.insert("severity".into(), "error".into());
        assert!(is_visible_failure_status_detail(Some(&JsValue::Object(
            detail.clone()
        ))));
        detail.insert("visible".into(), JsValue::Bool(false));
        assert!(!is_visible_failure_status_detail(Some(&JsValue::Object(
            detail
        ))));
        let mut warn = JsObject::new();
        warn.insert("severity".into(), "warning".into());
        assert!(!is_visible_failure_status_detail(Some(&JsValue::Object(
            warn
        ))));
        assert!(!is_visible_failure_status_detail(None));
        assert!(!is_visible_failure_status_detail(Some(&JsValue::String(
            "x".into()
        ))));
    }

    #[test]
    fn status_message_falls_back_on_blank_or_missing() {
        assert_eq!(
            visible_status_message(None, "fallback"),
            "fallback",
            "#1334 missing detail"
        );
        let mut blank = JsObject::new();
        blank.insert("message".into(), "   ".into());
        assert_eq!(
            visible_status_message(Some(&JsValue::Object(blank)), "fallback"),
            "fallback",
            "#1334 blank message"
        );
        let mut present = JsObject::new();
        present.insert("message".into(), "kept".into());
        assert_eq!(
            visible_status_message(Some(&JsValue::Object(present)), "fallback"),
            "kept",
            "#1334 present message"
        );
    }

    /// The combined predicate both renderers previously duplicated
    /// (#1615): hidden by `visible: false`, visible by the event-name set
    /// with or without a detail, visible by an error-severity detail.
    #[test]
    fn agent_status_visibility_matches_the_event_set_and_detail() {
        fn agent_status(event: &str, detail: Option<JsObject>) -> JsValue {
            let mut object = JsObject::new();
            object.insert("type".into(), "agent_status".into());
            object.insert("event".into(), event.into());
            if let Some(detail) = detail {
                object.insert("detail".into(), JsValue::Object(detail));
            }
            JsValue::Object(object)
        }
        let mut hidden = JsObject::new();
        hidden.insert("visible".into(), JsValue::Bool(false));
        assert!(
            !is_visible_failure_agent_status_event(&agent_status("session_busy", Some(hidden))),
            "hidden by visible:false even with a set name"
        );
        assert!(
            is_visible_failure_agent_status_event(&agent_status("session_busy", None)),
            "in the set without a detail"
        );
        let mut quiet = JsObject::new();
        quiet.insert("severity".into(), "warning".into());
        assert!(
            !is_visible_failure_agent_status_event(&agent_status("quiet_progress", Some(quiet))),
            "not in the set and not an error-severity detail"
        );
        let mut error_detail = JsObject::new();
        error_detail.insert("severity".into(), "error".into());
        assert!(
            is_visible_failure_agent_status_event(&agent_status("not_in_set", Some(error_detail))),
            "an error-severity detail is visible without a set name"
        );
    }
}
