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
                // #2692 (DS 全量块 sibling#2): TS keys the grant Map by the
                // raw sessionKey, and the turn side now acquires in the router
                // domain (`native_process/assembly.rs` acquires by
                // `context.session_key`, the `router_key` tag), so the grant
                // must land under the same key — `5` / `null` / `true` /
                // missing each become their own tagged bucket that exactly the
                // turn born under that raw value reads, and a `""` grant now
                // only matches a genuinely-`""`-keyed turn (the old
                // skip-the-empty-string argument died with the single-domain
                // acquire: a turn without a sessionKey acquires
                // `\0\0raw:undefined`, never `""`). A spelling that throws (a
                // too-deep array) is a reference key in TS; Rust cannot store
                // it, so it is skipped like the other read faces (registered
                // R15 divergence family). The write face inherits R15 差异①
                // 的标签碰撞/结构键控 unchanged (#2960 fix-r6 增量
                // boundary#7): the grant Map is one more router-shaped map,
                // so a literal `"\0\0raw:null"` string grant collides with
                // the null grant's tag exactly like the read faces do (no
                // new divergence, no upgrade to the registration). The
                // response keeps the TS probe
                // shape (`{granted:true, entry}`) either way
                // (`coordinator.rs:grant_session_permission`).
                let owner = crate::js_value::router_key_of(params.get("sessionKey")).ok();
                Ok(self
                    .interactions
                    .grant_session_permission(owner.as_ref(), &entry))
            }
        })
    }
}
