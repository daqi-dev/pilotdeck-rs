//! Keep the evaluator error typed until the reserved tool's legacy projection.
//! Generic HostError is not used as an untyped carrier for retryability.
use super::super::client::ClientError;
use crate::host::HostError;
use pilotdeck_protocol::{JsObject, JsString, JsValue};
pub(super) enum ControlFailure {
    Host(Box<HostError>),
    Client(Box<ClientError>),
}
impl From<HostError> for ControlFailure {
    fn from(value: HostError) -> Self {
        Self::Host(Box::new(value))
    }
}
impl ControlFailure {
    pub(super) fn client(value: ClientError) -> Self {
        Self::Client(Box::new(value))
    }
    pub(super) fn describe(self) -> Description {
        let (code, message, retry, owner) = match self {
            Self::Client(e) => (
                e.code,
                e.message,
                e.retryability.unwrap_or_else(|| "unsafe".into()),
                e.details.map(|v| *v),
            ),
            Self::Host(e) => {
                let code = e
                    .module_error
                    .as_ref()
                    .and_then(|m| m.code.clone())
                    .unwrap_or_else(|| "SOP_RUNTIME_REJECTED".into());
                let message = e
                    .module_error
                    .as_ref()
                    .map(|m| m.message.clone())
                    .unwrap_or_else(|| e.message.as_str().into());
                (code, message, "unsafe".into(), None)
            }
        };
        let owner = owner.unwrap_or_else(|| JsValue::Object(JsObject::new()));
        let mut details = owner.as_object().cloned().unwrap_or_default();
        details.insert(
            "sopRuntime".into(),
            crate::metadata::object([
                ("code", JsValue::String(code.clone())),
                ("message", JsValue::String(message.clone())),
                ("retryability", JsValue::String(retry)),
                ("details", owner),
            ]),
        );
        let tool_code = if code.equals_str("SOP_RUNTIME_CANCELLED") {
            "tool_aborted"
        } else {
            "tool_execution_failed"
        };
        Description {
            code,
            message,
            tool_code,
            details: JsValue::Object(details),
        }
    }
}
pub(super) struct Description {
    pub code: JsString,
    pub message: JsString,
    pub tool_code: &'static str,
    pub details: JsValue,
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn eight_actual_ts_error_descriptions_reach_the_real_control_result() {
        let fixture = pilotdeck_protocol::parse_json(include_str!(
            "../../../tests/fixtures/sop-presentation/oracle.json"
        ))
        .unwrap();
        let JsValue::Array(cases) = fixture.get("errors").unwrap() else {
            panic!("errors")
        };
        assert_eq!(cases.len(), 8);
        for row in cases {
            let message = row.get("message").unwrap().as_string().unwrap().clone();
            let code = row.get("code").and_then(JsValue::as_string).cloned();
            let failure = if row.get("client") == Some(&JsValue::Bool(true)) {
                ControlFailure::client(ClientError {
                    code: code.unwrap(),
                    message,
                    details: row.get("details").cloned().map(Box::new),
                    retryability: row
                        .get("retryability")
                        .and_then(JsValue::as_string)
                        .cloned(),
                })
            } else {
                let mut error = crate::host::ModuleError::new(message);
                error.code = code;
                ControlFailure::from(HostError::from(error))
            };
            let call = crate::host::modules::capability::ToolCall {
                id: "owned-call".into(),
                name: "submit_step_result".into(),
                input: JsValue::Null,
            };
            let actual = super::super::failure_result(&call, failure);
            let expected = row.get("expected").unwrap();
            let fields = actual.get("error").unwrap();
            assert_eq!(
                fields.get("code"),
                expected.get("toolCode"),
                "{:?}",
                row.get("name")
            );
            assert_eq!(
                fields.get("details"),
                expected.get("details"),
                "{:?}",
                row.get("name")
            );
            let mut message = vec![91];
            message.extend_from_slice(expected.get("code").unwrap().as_string().unwrap().units());
            message.extend("] ".encode_utf16());
            message.extend_from_slice(
                expected
                    .get("message")
                    .unwrap()
                    .as_string()
                    .unwrap()
                    .units(),
            );
            let message = JsValue::String(JsString::from_utf16(message));
            assert_eq!(
                fields.get("message"),
                Some(&message),
                "{:?}",
                row.get("name")
            );
            let JsValue::Array(content) = actual.get("content").unwrap() else {
                panic!("text content")
            };
            assert_eq!(content[0].get("text"), Some(&message));
            assert_eq!(actual.get("type"), Some(&JsValue::from("error")));
            assert_eq!(actual.get("toolCallId"), Some(&JsValue::from("owned-call")));
            assert_eq!(
                actual.get("toolName"),
                Some(&JsValue::from("submit_step_result"))
            );
        }
    }
}
