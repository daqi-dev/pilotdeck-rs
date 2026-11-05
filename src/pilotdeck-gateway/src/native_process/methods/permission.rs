use super::super::requests::Requests;
use crate::runtime::connection::{Call, RequestError};
use pilotdeck_protocol::{JsObject, JsString, JsValue};

pub(super) const METHODS: &[&str] = &["permission_list", "grant_session_permission"];
pub(super) const HANDLER: super::Handler =
    super::Handler::AfterAdmission(Requests::permission_request);

/// Domain errors carry their code in `RequestError::code` (like the
/// user_dialog family), not embedded in the message: clients that branch on
/// `error.code` must see the domain code, not the generic
/// `gateway_request_failed` default.
fn permission_error(code: &str, message: &str) -> RequestError {
    RequestError {
        code: code.into(),
        message: message.into(),
        details: None,
        validation: None,
    }
}

/// TS listPermissions projection (InProcessGateway.ts:2784-2789): the three
/// id fields fall back to `""` when the snapshot row lacks them (`?? ""`), and
/// `payload` is omitted when undefined. A present null payload is forwarded.
pub(super) fn project_permission_entry(entry: &JsValue) -> JsValue {
    let mut obj = JsObject::new();
    for key in ["requestId", "toolCallId", "toolName"] {
        obj.insert(
            key.into(),
            entry
                .get(key)
                .cloned()
                .unwrap_or_else(|| JsValue::String(JsString::from(""))),
        );
    }
    if let Some(payload) = entry.get("payload")
        && !payload.is_undefined()
    {
        obj.insert("payload".into(), payload.clone());
    }
    JsValue::Object(obj)
}

impl Requests {
    pub(in crate::native_process) fn permission_request(
        &self,
        method: String,
        params: JsValue,
    ) -> Call<'_, Result<JsValue, RequestError>> {
        Box::pin(async move {
            if method == "permission_list" {
                // TS: listPermissions — sessionKey required
                let session_key = params
                    .get("sessionKey")
                    .and_then(JsValue::as_string)
                    .filter(|s| {
                        let text = String::from_utf16_lossy(s.units());
                        !crate::metadata::js_trim(&text).is_empty()
                    })
                    .cloned()
                    .ok_or_else(|| {
                        // TS: DialogGatewayError("INVALID_PERMISSION_LIST",
                        // "sessionKey is required.") — InProcessGateway.ts:2782.
                        permission_error("INVALID_PERMISSION_LIST", "sessionKey is required.")
                    })?;

                // permission_bus snapshot → project to { requestId, toolCallId, toolName, payload? }
                let raw = self.interactions.permission_bus().snapshot(&session_key);
                let requests: Vec<JsValue> = raw
                    .into_iter()
                    .map(|entry| project_permission_entry(&entry))
                    .collect();
                Ok(crate::metadata::object([(
                    "requests",
                    JsValue::Array(requests),
                )]))
            } else {
                // grant_session_permission
                // TS: grantSessionPermission(input) → coordinator.grantSessionPermission(input)
                // input has sessionKey (owner) and entry (permission rule string)
                let owner = params
                    .get("sessionKey")
                    .and_then(JsValue::as_string)
                    .filter(|s| {
                        let text = String::from_utf16_lossy(s.units());
                        !crate::metadata::js_trim(&text).is_empty()
                    })
                    .cloned()
                    .ok_or_else(|| {
                        // TS grantSessionPermission performs no input validation
                        // (GatewayInteractionCoordinator.ts:128); these rejects
                        // enforce the schema's required sessionKey/entry with a
                        // domain code instead of the generic transport code.
                        permission_error("INVALID_PERMISSION_GRANT", "sessionKey is required.")
                    })?;
                let entry = params
                    .get("entry")
                    .and_then(JsValue::as_string)
                    .cloned()
                    .ok_or_else(|| {
                        permission_error("INVALID_PERMISSION_GRANT", "entry must be a string.")
                    })?;
                Ok(self.interactions.grant_session_permission(&owner, &entry))
            }
        })
    }
}
