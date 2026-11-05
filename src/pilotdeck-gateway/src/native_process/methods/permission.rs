use super::super::requests::Requests;
use crate::runtime::connection::{Call, RequestError};
use pilotdeck_protocol::{JsObject, JsString, JsValue};

pub(super) const METHODS: &[&str] = &["permission_list", "grant_session_permission"];
pub(super) const HANDLER: super::Handler =
    super::Handler::AfterAdmission(Requests::permission_request);

/// TS listPermissions projection (InProcessGateway.ts:2784-2790).
///
/// The TS object literal treats the three id fields differently:
/// `toolCallId ?? ""` and `toolName ?? ""` fold only null/undefined to `""`,
/// while `requestId` has no fallback — a row without it drops the key from the
/// serialized frame. Rust fills all three with `""`. That difference is
/// unreachable on the wired path: permission-bus snapshot rows always carry
/// string `requestId` / `toolCallId` / `toolName`
/// (`runtime/interaction/reconnect.rs:194-206` builds all three with
/// `JsValue::String`), so a missing or non-string id cannot occur here (#1465).
///
/// `payload` is omitted when undefined; a present null payload is forwarded.
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
                // TS: `if (!input.sessionKey?.trim())` — a nullish params
                // object or a non-string sessionKey throws at the boundary
                // (gateway_request_failed), while null/undefined/blank is the
                // domain error (InProcessGateway.ts:2781-2783).
                let session_key = super::require_text(
                    &params,
                    "sessionKey",
                    "INVALID_PERMISSION_LIST",
                    "sessionKey is required.",
                )?;

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
                // (InProcessGateway.ts:2837, GatewayInteractionCoordinator.ts:128).
                // The coordinator performs NO input validation:
                // `permissionEntryToRule(input.entry, ...)` reads `entry` first,
                // so a nullish params object throws on 'entry' (not
                // 'sessionKey'), and `entry.trim()` throws its own TypeError
                // for nullish / non-string entries. Only after that does
                // `permissionGrants.grant(input.sessionKey, rule)` accept any
                // sessionKey value as a Map key (#1465 TS probes).
                let entry = match super::input_property(&params, "entry")?.as_ref() {
                    None | Some(JsValue::Undefined) => {
                        return Err(RequestError::new(
                            "Cannot read properties of undefined (reading 'trim')",
                        ));
                    }
                    Some(JsValue::Null) => {
                        return Err(RequestError::new(
                            "Cannot read properties of null (reading 'trim')",
                        ));
                    }
                    Some(JsValue::String(value)) => value.clone(),
                    Some(_) => return Err(RequestError::new("entry.trim is not a function")),
                };
                // TS keys the grant Map by the raw sessionKey. A non-string key
                // can never be reached again through the string-keyed
                // permission RPCs, so it collapses to the empty owner here and
                // the observable response matches `{granted:true, entry}`.
                let owner = match params.get("sessionKey") {
                    Some(JsValue::String(value)) => value.clone(),
                    _ => JsString::from(""),
                };
                Ok(self.interactions.grant_session_permission(&owner, &entry))
            }
        })
    }
}
