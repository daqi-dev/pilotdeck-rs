//! `src/status/agentStatus.ts` (baseline cc7c4e6), the slice the webhook
//! channel consumes. Key insertion order is the JS object literal order, so
//! the JSON bodies stringify identically.
use pilotdeck_protocol::{JsObject, JsString, JsValue};

/// `createVisibleErrorStatusDetail`: severity defaults to `"error"`, visible
/// is always true, then the caller's `detail` spread.
pub fn create_visible_error_status_detail(input: StatusDetailInput) -> JsObject {
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

/// `createAgentStatusHttpErrorBody` for the webhook source: `{error, agent_status}`.
pub fn create_agent_status_http_error_body(input: HttpErrorBodyInput) -> JsValue {
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
        source: "webhook",
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
    error.insert("source".into(), JsValue::String("webhook".into()));
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
pub fn is_visible_failure_status_detail(detail: Option<&JsValue>) -> bool {
    let Some(object) = detail.and_then(JsValue::as_object) else {
        return false;
    };
    object
        .get("visible")
        .is_none_or(|value| *value != JsValue::Bool(false))
        && object.get("severity") == Some(&JsValue::String("error".into()))
}

/// `visibleStatusMessage`: the detail's non-blank string message, else the
/// fallback. Blankness uses the JS trim (U+FEFF included).
pub fn visible_status_message(detail: Option<&JsValue>, fallback: &str) -> String {
    if let Some(message) = detail
        .and_then(JsValue::as_object)
        .and_then(|object| object.get("message"))
        .and_then(JsValue::as_string)
        .map(|text| String::from_utf16_lossy(text.units()))
        && !crate::metadata::js_trim(&message).is_empty()
    {
        return message;
    }
    fallback.to_owned()
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

pub type Scope = &'static str;
pub type Severity = &'static str;
pub type Source = &'static str;

pub struct StatusDetailInput {
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

pub struct HttpErrorBodyInput {
    pub event: JsString,
    pub message: JsString,
    pub code: Option<JsValue>,
    pub status: Option<u16>,
    pub r#type: Option<JsValue>,
    pub user_hint: Option<JsValue>,
    pub scope: Scope,
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
}
