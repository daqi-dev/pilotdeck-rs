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
                    let session_key = super::require_text(
                        &params,
                        "sessionKey",
                        "INVALID_USER_DIALOG_LIST",
                        "sessionKey is required.",
                    )?;
                    // #1442: `list` may expire a lease and queue a change;
                    // the observer is host code, so it is delivered only
                    // after the guard drops.
                    let (dialogs, outbox) = {
                        let mut bus = self
                            .user_dialog_bus
                            .lock()
                            .unwrap_or_else(|e| e.into_inner());
                        let dialogs = bus.list(&session_key);
                        let outbox = bus.drain_events();
                        (dialogs, outbox)
                    };
                    outbox.deliver();
                    Ok(crate::metadata::object([(
                        "dialogs",
                        JsValue::Array(dialogs),
                    )]))
                }

                "user_dialog_claim" => {
                    // TS: claimUserDialog — CAPABILITY_UNAVAILABLE when bus is not wired
                    // (native process mode; GatewayWsConnection.ts:481).
                    self.require_user_dialog_bus()?;
                    // TS evaluates `!input.sessionKey?.trim() ||
                    // !input.requestId?.trim()` left to right
                    // (InProcessGateway.ts:2735-2737): a non-string
                    // sessionKey throws before requestId is read, and a
                    // missing sessionKey short-circuits the whole guard.
                    let session_key = super::require_text(
                        &params,
                        "sessionKey",
                        "INVALID_USER_DIALOG_CLAIM",
                        "sessionKey and requestId are required.",
                    )?;
                    let request_id = super::require_text(
                        &params,
                        "requestId",
                        "INVALID_USER_DIALOG_CLAIM",
                        "sessionKey and requestId are required.",
                    )?;
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
                    // #1442: `claim` queues lease changes (including a
                    // just-expired one); the observer is host code, so the
                    // outbox is delivered only after the guard drops — on
                    // every outcome, since the expiry broadcast happened
                    // before the claim result was known.
                    let (claim, outbox) = {
                        let mut bus = self
                            .user_dialog_bus
                            .lock()
                            .unwrap_or_else(|e| e.into_inner());
                        let claim = bus.claim(&session_key, &request_id, ttl_ms, lease_id.as_ref());
                        let outbox = bus.drain_events();
                        (claim, outbox)
                    };
                    outbox.deliver();
                    let claim = claim.map_err(|e| request_error(e.to_string()))?;
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
                    let session_key = super::require_text(
                        &params,
                        "sessionKey",
                        "INVALID_USER_DIALOG_RELEASE",
                        "sessionKey, requestId, and leaseId are required.",
                    )?;
                    let request_id = super::require_text(
                        &params,
                        "requestId",
                        "INVALID_USER_DIALOG_RELEASE",
                        "sessionKey, requestId, and leaseId are required.",
                    )?;
                    // TS release has NO typeof short-circuit on leaseId
                    // (InProcessGateway.ts:2766-2769, unlike claim :2738 /
                    // respond :2655): `!input.leaseId?.trim()` guards only
                    // null/undefined through optional chaining, so any other
                    // non-string leaseId calls `.trim` on the raw value and
                    // throws the boundary TypeError — exactly `require_text`'s
                    // three-way split. The nullish-`params` arm differs on
                    // paper (property-read TypeError vs domain error) but is
                    // unreachable: the `sessionKey` guard above runs first.
                    // Wire probe (_tmp/20261105-issue-1309-fix-r7/ts-probe/):
                    // number, object, array, and boolean leaseIds all yield
                    // {code:"gateway_request_failed",
                    // message:"input.leaseId?.trim is not a function"}.
                    let lease_id = super::require_text(
                        &params,
                        "leaseId",
                        "INVALID_USER_DIALOG_RELEASE",
                        "sessionKey, requestId, and leaseId are required.",
                    )?;
                    // TS passes `input.leaseId.trim()` into the bus
                    // (InProcessGateway.ts:2769) — sessionKey/requestId stay
                    // raw because TS forwards them untrimmed.
                    let lease_id = trimmed(&lease_id);
                    // #1442: `release` queues the "released" (or a stale
                    // lease's "expired") change; deliver after the guard drops.
                    let (released, outbox) = {
                        let mut bus = self
                            .user_dialog_bus
                            .lock()
                            .unwrap_or_else(|e| e.into_inner());
                        let released = bus.release(&session_key, &request_id, &lease_id);
                        let outbox = bus.drain_events();
                        (released, outbox)
                    };
                    outbox.deliver();
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
                    let session_key = super::require_text(
                        &params,
                        "sessionKey",
                        "INVALID_USER_DIALOG_RESPONSE",
                        "sessionKey and requestId are required.",
                    )?;
                    let request_id = super::require_text(
                        &params,
                        "requestId",
                        "INVALID_USER_DIALOG_RESPONSE",
                        "sessionKey and requestId are required.",
                    )?;
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

                    // TS validates result/behavior/value/reason before leaseId
                    // (InProcessGateway.ts:2646-2655), including when both are invalid.
                    let lease_id = optional_nonempty_string(&params, "leaseId").map_err(|_| {
                        domain_error(
                            "INVALID_USER_DIALOG_RESPONSE",
                            "leaseId must be a non-empty string when provided.",
                        )
                    })?;

                    // #1442: the bus guard covers only state reads and
                    // mutations. `accepts`, `on_settled`, `resolve` and the
                    // change observer are host code and run after the guard
                    // drops: TS is single-threaded and lets them reenter the
                    // bus freely (InProcessGateway.ts:2668-2707), so a
                    // MutexGuard must never be alive across them.
                    let (snapshot, outbox) = {
                        let mut bus = self
                            .user_dialog_bus
                            .lock()
                            .unwrap_or_else(|e| e.into_inner());
                        // Peek to validate accepts before consuming
                        // (InProcessGateway.ts:2668-2674); the validator
                        // itself runs outside the guard, on the snapshot's
                        // cloneable handle.
                        let snapshot = bus.snapshot_for_response(&session_key, &request_id);
                        let outbox = bus.drain_events();
                        (snapshot, outbox)
                    };
                    outbox.deliver();

                    let answered = answer.to_value().get("behavior") == Some(&"answered".into());
                    if answered && snapshot.is_none() {
                        // TS validates on the peeked entry and delivers
                        // nothing when there is none (InProcessGateway.ts:2668-2674).
                        // Falling through with no snapshot would consume and
                        // deliver to whatever a host callback registered under
                        // the same request id since the peek, unvalidated
                        // (#1599).
                        return Ok(crate::metadata::object([(
                            "delivered",
                            JsValue::Bool(false),
                        )]));
                    }
                    if answered && let Some(snapshot) = &snapshot {
                        let value = result_val
                            .get("value")
                            .cloned()
                            .unwrap_or(JsValue::Undefined);
                        if !(snapshot.accepts)(&value) {
                            return Err(RequestError {
                                    code: "INVALID_USER_DIALOG_RESPONSE".into(),
                                    message: format!(
                                        "answered {} dialog result does not match the pending dialog contract.",
                                        String::from_utf16_lossy(snapshot.dialog_kind.units())
                                    ).into(),
                                    details: None,
                                    validation: None,
                                });
                        }
                    }

                    let reason_str = if answered { "answered" } else { "cancelled" };

                    // TS hands the bus `input.leaseId?.trim()`
                    // (InProcessGateway.ts:2683), not the raw validated string.
                    let lease_id = lease_id.as_ref().map(trimmed);
                    let (consumed, outbox) = {
                        let mut bus = self
                            .user_dialog_bus
                            .lock()
                            .unwrap_or_else(|e| e.into_inner());
                        let consumed = bus.consume_for_response(
                            &session_key,
                            &request_id,
                            lease_id.as_ref(),
                            reason_str,
                            // Only an answered response was validated against
                            // the snapshot; a cancel removes whatever is
                            // pending, like TS (#1599).
                            if answered { snapshot.as_ref() } else { None },
                        );
                        let outbox = bus.drain_events();
                        (consumed, outbox)
                    };
                    match consumed {
                        ConsumeResult::NotPending => {
                            outbox.deliver();
                            Ok(crate::metadata::object([(
                                "delivered",
                                JsValue::Bool(false),
                            )]))
                        }
                        ConsumeResult::Replaced => {
                            // A host callback reentered the bus between the
                            // snapshot and this consume and put a different
                            // dialog under the same request id (#1599). The
                            // answer was validated against the old one, so it
                            // must not be delivered to the new one.
                            outbox.deliver();
                            Ok(crate::metadata::object([(
                                "delivered",
                                JsValue::Bool(false),
                            )]))
                        }
                        ConsumeResult::LeaseRequired => {
                            outbox.deliver();
                            Err(RequestError {
                                code: "USER_DIALOG_LEASE_REQUIRED".into(),
                                message: format!(
                                    "User dialog {} is currently claimed by another renderer.",
                                    String::from_utf16_lossy(request_id.units())
                                )
                                .into(),
                                details: None,
                                validation: None,
                            })
                        }
                        ConsumeResult::Entry(consumed) => {
                            // #1442: the settle hook runs outside the guard.
                            // A failure restores the consumed entry (with its
                            // lease) and discards the queued "settled"
                            // broadcast — the TS remove() fail-closed
                            // contract (GatewayUserDialogBus.ts:219-226).
                            let settled = consumed
                                .entry
                                .on_settled
                                .as_ref()
                                .map_or(Ok(()), |hook| hook());
                            if let Err(error) = settled {
                                let mut bus = self
                                    .user_dialog_bus
                                    .lock()
                                    .unwrap_or_else(|e| e.into_inner());
                                bus.restore(&session_key, *consumed);
                                drop(bus);
                                return Err(request_error(error.to_string()));
                            }
                            // TS remove() broadcasts before the waiter
                            // resolves (InProcessGateway.ts:2678-2707).
                            outbox.deliver();
                            (consumed.entry.resolve)(answer);
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
