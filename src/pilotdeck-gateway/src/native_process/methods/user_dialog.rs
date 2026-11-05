use super::super::requests::Requests;
use crate::{
    runtime::connection::{Call, RequestError},
    user_dialog::{ConsumeResult, DialogAnswer},
};
use pilotdeck_protocol::{JsString, JsValue};

pub(super) const METHODS: &[&str] = &[
    "user_dialog_list",
    "user_dialog_claim",
    "user_dialog_release",
    "user_dialog_respond",
];
pub(super) const HANDLER: super::Handler =
    super::Handler::AfterAdmission(Requests::user_dialog_request);

// TS constants: DEFAULT_USER_DIALOG_LEASE_MS = 30_000
//               MIN_USER_DIALOG_LEASE_MS     =  1_000
//               MAX_USER_DIALOG_LEASE_MS     = 300_000 (5 * 60_000)
const DEFAULT_LEASE_MS: f64 = 30_000.0;
const MIN_LEASE_MS: f64 = 1_000.0;
const MAX_LEASE_MS: f64 = 300_000.0;

fn request_error(message: impl Into<pilotdeck_protocol::JsString>) -> RequestError {
    RequestError::new(message)
}

fn capability_unavailable() -> RequestError {
    RequestError {
        code: "CAPABILITY_UNAVAILABLE".into(),
        message: "User dialog bus is unavailable in native-process mode.".into(),
        details: None,
        validation: None,
    }
}

fn domain_error(code: &str, message: impl Into<pilotdeck_protocol::JsString>) -> RequestError {
    RequestError {
        code: code.into(),
        message: message.into(),
        details: None,
        validation: None,
    }
}

fn nonempty_string(params: &JsValue, key: &str) -> Result<JsString, RequestError> {
    params
        .get(key)
        .and_then(JsValue::as_string)
        .filter(|s| {
            let text = String::from_utf16_lossy(s.units());
            !crate::metadata::js_trim(&text).is_empty()
        })
        .cloned()
        .ok_or_else(|| request_error(format!("{key} must be a non-empty string.")))
}

/// Optional non-empty string (used for `leaseId` on claim/respond). TS rejects
/// an explicit null here, so it must NOT fold into the missing-key default:
/// the guard is `input.leaseId !== undefined && (typeof input.leaseId !==
/// "string" || !input.leaseId.trim())` (claim: InProcessGateway.ts:2738,
/// respond: InProcessGateway.ts:2655) — a JS-serialized `{leaseId: x ?? null}`
/// keeps the domain validation error, unlike `ttlMs` whose `??` (:2741) does
/// fold null into the default. The RAW validated string is returned: like TS,
/// which trims for the `!input.leaseId.trim()` guard but hands
/// `input.leaseId?.trim()` to the bus at each call site (claim
/// InProcessGateway.ts:2754, respond :2683), the trim happens where the value
/// is used, so a padded `" <uuid> "` renews the same lease instead of
/// colliding with it.
fn optional_nonempty_string(params: &JsValue, key: &str) -> Result<Option<JsString>, RequestError> {
    match params.get(key) {
        None | Some(JsValue::Undefined) => Ok(None),
        Some(JsValue::String(s)) => {
            let text = String::from_utf16_lossy(s.units());
            if crate::metadata::js_trim(&text).is_empty() {
                Err(request_error(format!(
                    "{key} must be a non-empty string when provided."
                )))
            } else {
                Ok(Some(s.clone()))
            }
        }
        _ => Err(request_error(format!(
            "{key} must be a non-empty string when provided."
        ))),
    }
}

/// `input.leaseId?.trim()` / `.trim()` as handed to the bus at the claim
/// (InProcessGateway.ts:2754), respond (:2683), and release (:2769) call sites.
fn trimmed(id: &JsString) -> JsString {
    JsString::from(crate::metadata::js_trim(&String::from_utf16_lossy(
        id.units(),
    )))
}

impl Requests {
    /// Shared capability gate for `user_dialog_list` / `_claim` / `_release`:
    /// the bus must be wired before any parameter validation, matching the TS
    /// order (GatewayWsConnection.ts:481 checks availability first).
    fn require_user_dialog_bus(&self) -> Result<(), RequestError> {
        let bus = self
            .user_dialog_bus
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if !bus.is_connected() {
            return Err(capability_unavailable());
        }
        Ok(())
    }

    pub(in crate::native_process) fn user_dialog_request(
        &self,
        method: String,
        params: JsValue,
    ) -> Call<'_, Result<JsValue, RequestError>> {
        Box::pin(async move {
            match method.as_str() {
                "user_dialog_list" => {
                    // TS: listUserDialogs — CAPABILITY_UNAVAILABLE when bus is not wired
                    // (native process mode; GatewayWsConnection.ts:481).
                    self.require_user_dialog_bus()?;
                    let session_key = nonempty_string(&params, "sessionKey").map_err(|_| {
                        domain_error("INVALID_USER_DIALOG_LIST", "sessionKey is required.")
                    })?;
                    let dialogs = self
                        .user_dialog_bus
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .list(&session_key);
                    Ok(crate::metadata::object([(
                        "dialogs",
                        JsValue::Array(dialogs),
                    )]))
                }

                "user_dialog_claim" => {
                    // TS: claimUserDialog — CAPABILITY_UNAVAILABLE when bus is not wired
                    // (native process mode; GatewayWsConnection.ts:481).
                    self.require_user_dialog_bus()?;
                    let session_key = nonempty_string(&params, "sessionKey").map_err(|_| {
                        domain_error(
                            "INVALID_USER_DIALOG_CLAIM",
                            "sessionKey and requestId are required.",
                        )
                    })?;
                    let request_id = nonempty_string(&params, "requestId").map_err(|_| {
                        domain_error(
                            "INVALID_USER_DIALOG_CLAIM",
                            "sessionKey and requestId are required.",
                        )
                    })?;
                    let lease_id = optional_nonempty_string(&params, "leaseId").map_err(|_| {
                        domain_error(
                            "INVALID_USER_DIALOG_CLAIM",
                            "leaseId must be a non-empty string when provided.",
                        )
                    })?;
                    let ttl_ms = match params.get("ttlMs") {
                        // TS: `input.ttlMs ?? DEFAULT_USER_DIALOG_LEASE_MS`
                        // (InProcessGateway.ts:2741) — `??` also covers null, so
                        // an explicit null claims with the default 30s lease.
                        None | Some(JsValue::Undefined) | Some(JsValue::Null) => DEFAULT_LEASE_MS,
                        Some(JsValue::Number(n)) => *n,
                        _ => {
                            return Err(domain_error(
                                "INVALID_USER_DIALOG_CLAIM",
                                format!(
                                    "ttlMs must be a safe integer between {MIN_LEASE_MS} and {MAX_LEASE_MS}."
                                ),
                            ));
                        }
                    };
                    if !ttl_ms.is_finite()
                        || ttl_ms < MIN_LEASE_MS
                        || ttl_ms > MAX_LEASE_MS
                        || ttl_ms.fract() != 0.0
                    {
                        return Err(domain_error(
                            "INVALID_USER_DIALOG_CLAIM",
                            format!(
                                "ttlMs must be a safe integer between {MIN_LEASE_MS} and {MAX_LEASE_MS}."
                            ),
                        ));
                    }
                    // TS hands the bus `input.leaseId?.trim()`
                    // (InProcessGateway.ts:2754), not the raw validated string.
                    let lease_id = lease_id.as_ref().map(trimmed);
                    let claim = self
                        .user_dialog_bus
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .claim(&session_key, &request_id, ttl_ms, lease_id.as_ref())
                        .map_err(|e| request_error(e.to_string()))?;
                    use crate::user_dialog::LeaseClaim;
                    match claim {
                        LeaseClaim::Claimed(lease) => Ok(crate::metadata::object([
                            ("claimed", JsValue::Bool(true)),
                            ("leaseId", JsValue::String(lease.lease_id)),
                            ("expiresAt", JsValue::String(lease.expires_at)),
                        ])),
                        LeaseClaim::Busy { expires_at } => {
                            let mut obj = crate::metadata::object([
                                ("claimed", JsValue::Bool(false)),
                                ("reason", "claimed".into()),
                            ]);
                            if let JsValue::Object(ref mut o) = obj {
                                o.insert("expiresAt".into(), JsValue::String(expires_at));
                            }
                            Ok(obj)
                        }
                        LeaseClaim::NotPending => Ok(crate::metadata::object([
                            ("claimed", JsValue::Bool(false)),
                            ("reason", "not_pending".into()),
                        ])),
                    }
                }

                "user_dialog_release" => {
                    // TS: releaseUserDialog — CAPABILITY_UNAVAILABLE when bus is not wired
                    // (native process mode; GatewayWsConnection.ts:481).
                    self.require_user_dialog_bus()?;
                    let session_key = nonempty_string(&params, "sessionKey").map_err(|_| {
                        domain_error(
                            "INVALID_USER_DIALOG_RELEASE",
                            "sessionKey, requestId, and leaseId are required.",
                        )
                    })?;
                    let request_id = nonempty_string(&params, "requestId").map_err(|_| {
                        domain_error(
                            "INVALID_USER_DIALOG_RELEASE",
                            "sessionKey, requestId, and leaseId are required.",
                        )
                    })?;
                    // TS release has NO typeof short-circuit on leaseId
                    // (InProcessGateway.ts:2766-2769, unlike claim :2738 /
                    // respond :2655): `!input.leaseId?.trim()` only guards
                    // null/undefined through optional chaining, so any other
                    // non-string leaseId calls `.trim` on the raw value and
                    // throws a TypeError. GatewayWsConnection.ts:488 forwards
                    // the wire params with no schema interception, and the
                    // request boundary's generic catch serializes that
                    // TypeError as the transport failure. Wire probe
                    // (_tmp/20261105-issue-1309-fix-r7/ts-probe/): number,
                    // object, array, and boolean leaseIds all yield the
                    // identical frame {code:"gateway_request_failed",
                    // message:"input.leaseId?.trim is not a function"}.
                    let lease_id = match params.get("leaseId") {
                        None | Some(JsValue::Undefined) | Some(JsValue::Null) => {
                            return Err(domain_error(
                                "INVALID_USER_DIALOG_RELEASE",
                                "sessionKey, requestId, and leaseId are required.",
                            ));
                        }
                        Some(JsValue::String(s)) => {
                            let text = String::from_utf16_lossy(s.units());
                            if crate::metadata::js_trim(&text).is_empty() {
                                return Err(domain_error(
                                    "INVALID_USER_DIALOG_RELEASE",
                                    "sessionKey, requestId, and leaseId are required.",
                                ));
                            }
                            s.clone()
                        }
                        _ => {
                            return Err(request_error("input.leaseId?.trim is not a function"));
                        }
                    };
                    // TS passes `input.leaseId.trim()` into the bus
                    // (InProcessGateway.ts:2769) — sessionKey/requestId stay
                    // raw because TS forwards them untrimmed.
                    let lease_id = trimmed(&lease_id);
                    let released = self
                        .user_dialog_bus
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .release(&session_key, &request_id, &lease_id);
                    Ok(crate::metadata::object([(
                        "released",
                        JsValue::Bool(released),
                    )]))
                }

                _ => {
                    // user_dialog_respond
                    // TS: respondUserDialog — unconfigured bus has no pending entries;
                    // consume_for_response returns NotPending → {delivered:false} (ledger:7177/7178).
                    // TS: respondUserDialog — sessionKey, requestId required; result.behavior + value/reason; optional leaseId
                    let session_key = nonempty_string(&params, "sessionKey").map_err(|_| {
                        domain_error(
                            "INVALID_USER_DIALOG_RESPONSE",
                            "sessionKey and requestId are required.",
                        )
                    })?;
                    let request_id = nonempty_string(&params, "requestId").map_err(|_| {
                        domain_error(
                            "INVALID_USER_DIALOG_RESPONSE",
                            "sessionKey and requestId are required.",
                        )
                    })?;
                    let lease_id = optional_nonempty_string(&params, "leaseId").map_err(|_| {
                        domain_error(
                            "INVALID_USER_DIALOG_RESPONSE",
                            "leaseId must be a non-empty string when provided.",
                        )
                    })?;

                    let result_val = params.get("result").ok_or_else(|| {
                        domain_error(
                            "INVALID_USER_DIALOG_RESPONSE",
                            "result.behavior must be answered or cancelled.",
                        )
                    })?;

                    let behavior = result_val
                        .get("behavior")
                        .and_then(JsValue::as_string)
                        .ok_or_else(|| {
                            domain_error(
                                "INVALID_USER_DIALOG_RESPONSE",
                                "result.behavior must be answered or cancelled.",
                            )
                        })?
                        .clone();

                    let answer = if behavior.equals_str("answered") {
                        let value = result_val
                            .get("value")
                            .cloned()
                            .unwrap_or(JsValue::Undefined);
                        // TS only rejects a missing value:
                        // `input.result.value === undefined`
                        // (InProcessGateway.ts:2649). An explicit `value: null`
                        // passes this guard and is instead judged by the dialog
                        // contract via `entry.accepts(input.result.value)`
                        // (InProcessGateway.ts:2674), so `null` stays a valid
                        // answer whenever the pending dialog accepts it.
                        if value.is_undefined() {
                            return Err(domain_error(
                                "INVALID_USER_DIALOG_RESPONSE",
                                "answered user dialog results require a value.",
                            ));
                        }
                        DialogAnswer::Answered(value)
                    } else if behavior.equals_str("cancelled") {
                        // TS rejects an explicit null reason:
                        // `input.result.reason !== undefined && typeof
                        // input.result.reason !== "string"`
                        // (InProcessGateway.ts:2652) — only a missing or
                        // undefined reason is omitted.
                        let reason = match result_val.get("reason") {
                            None | Some(JsValue::Undefined) => None,
                            Some(JsValue::String(s)) => Some(s.clone()),
                            _ => {
                                return Err(domain_error(
                                    "INVALID_USER_DIALOG_RESPONSE",
                                    "cancelled user dialog reason must be a string.",
                                ));
                            }
                        };
                        DialogAnswer::Cancelled(reason)
                    } else {
                        return Err(domain_error(
                            "INVALID_USER_DIALOG_RESPONSE",
                            "result.behavior must be answered or cancelled.",
                        ));
                    };

                    let mut bus = self
                        .user_dialog_bus
                        .lock()
                        .unwrap_or_else(|e| e.into_inner());

                    // Peek to validate accepts before consuming
                    if answer.to_value().get("behavior") == Some(&"answered".into())
                        && let Some(entry) = bus.peek(&session_key, &request_id)
                    {
                        let value = result_val
                            .get("value")
                            .cloned()
                            .unwrap_or(JsValue::Undefined);
                        if !(entry.accepts)(&value) {
                            return Err(RequestError {
                                    code: "INVALID_USER_DIALOG_RESPONSE".into(),
                                    message: format!(
                                        "answered {} dialog result does not match the pending dialog contract.",
                                        String::from_utf16_lossy(entry.dialog_kind.units())
                                    ).into(),
                                    details: None,
                                    validation: None,
                                });
                        }
                    }

                    let reason_str =
                        if answer.to_value().get("behavior") == Some(&"answered".into()) {
                            "answered"
                        } else {
                            "cancelled"
                        };

                    // TS hands the bus `input.leaseId?.trim()`
                    // (InProcessGateway.ts:2683), not the raw validated string.
                    let lease_id = lease_id.as_ref().map(trimmed);
                    match bus
                        .consume_for_response(
                            &session_key,
                            &request_id,
                            lease_id.as_ref(),
                            reason_str,
                        )
                        .map_err(|e| request_error(e.to_string()))?
                    {
                        ConsumeResult::NotPending => Ok(crate::metadata::object([(
                            "delivered",
                            JsValue::Bool(false),
                        )])),
                        ConsumeResult::LeaseRequired => Err(RequestError {
                            code: "USER_DIALOG_LEASE_REQUIRED".into(),
                            message: format!(
                                "User dialog {} is currently claimed by another renderer.",
                                String::from_utf16_lossy(request_id.units())
                            )
                            .into(),
                            details: None,
                            validation: None,
                        }),
                        ConsumeResult::Entry(entry) => {
                            (entry.resolve)(answer);
                            Ok(crate::metadata::object([(
                                "delivered",
                                JsValue::Bool(true),
                            )]))
                        }
                    }
                }
            }
        })
    }
}
