//! Wire projection of the six cron methods (the `#1326` wiring surface, not
//! wired into `dispatch.rs::LOCAL_METHODS` here): the method-name table,
//! the `requireCron` gate, and the response envelope. Ownership per the TS
//! layers — the capabilities slice (InProcessGateway.ts:2114-2122), the
//! unconfigured gate (InProcessGateway.ts:3016-3022), and the error
//! serialization branches (GatewayWsConnection.ts:223-255) — none of which
//! validate parameters (that stays in `methods.rs`/`manager.rs`).
use pilotdeck_protocol::{JsValue, stringify_json};

use super::{
    CronError, CronManager, cron_create, cron_delete, cron_list, cron_run_now, cron_stop,
    cron_update,
};

/// The six cron control names, exactly as InProcessGateway.ts:2114-2122
/// splices them into the capabilities list (`options.cron` configured) and
/// GatewayWsConnection.ts:464-475 routes them.
pub const CRON_METHODS: [&str; 6] = [
    "cron_create",
    "cron_list",
    "cron_update",
    "cron_delete",
    "cron_stop",
    "cron_run_now",
];

/// `requireCron()` (InProcessGateway.ts:3016-3022): an unconfigured cron
/// runtime throws before any adapter runs. `None` models the missing
/// `options.cron`; the error is a `DialogGatewayError`, so it crosses the
/// wire with its own code instead of the `gateway_request_failed` fallback.
pub fn require_cron(controller: Option<&CronManager>) -> Result<&CronManager, CronError> {
    controller.ok_or_else(|| {
        CronError::gateway(
            "CAPABILITY_UNAVAILABLE",
            "Cron runtime is not configured on this gateway.",
        )
    })
}

/// The WS dispatch slice for the six names (GatewayWsConnection.ts:464-475
/// `this.options.gateway.cronCreate(frame.params as never)` over
/// InProcessGateway.ts:2603-2625): gate, then delegate to the adapter.
/// `None` means the method is not ours — the caller keeps routing.
pub fn dispatch(
    controller: Option<&CronManager>,
    method: &str,
    params: &JsValue,
) -> Option<Result<JsValue, CronError>> {
    if !CRON_METHODS.contains(&method) {
        return None;
    }
    let controller = match require_cron(controller) {
        Ok(controller) => controller,
        Err(error) => return Some(Err(error)),
    };
    Some(match method {
        "cron_create" => cron_create(controller, params),
        "cron_list" => cron_list(controller, params),
        "cron_update" => cron_update(controller, params),
        "cron_delete" => cron_delete(controller, params),
        "cron_stop" => cron_stop(controller, params),
        "cron_run_now" => cron_run_now(controller, params),
        _ => unreachable!("checked CRON_METHODS above"),
    })
}

/// GatewayWsConnection.ts:223-243 and 245-255: a `DialogGatewayError` (or any error
/// whose `code` is a string — the cron paths only ever produce the former)
/// keeps its code; every other thrown value falls back to
/// `gateway_request_failed`. The `SkillValidationError`/`SkillManagerError`
/// branches rank above both but are unreachable from cron; `details` and
/// the sop_status HTTP-status splice likewise never apply.
pub fn wire_error_code(error: &CronError) -> &str {
    error.code().unwrap_or("gateway_request_failed")
}

/// The response envelope GatewayWsConnection.ts:187 and 223-255 build:
/// `{type: "response", id, ok: true, result}` or
/// `{type: "response", id, ok: false, error: {code, message}}`. `None` is the
/// `JSON.stringify` codec failure the WS layer cannot send (the caller
/// falls back to forwarding), matching the other local-method responders.
pub fn response(request: &JsValue, outcome: Result<JsValue, CronError>) -> Option<Vec<u8>> {
    let id = request.get("id").cloned().unwrap_or(JsValue::Undefined);
    let frame = match outcome {
        Ok(result) => crate::metadata::object([
            ("type", "response".into()),
            ("id", id),
            ("ok", JsValue::Bool(true)),
            ("result", result),
        ]),
        Err(error) => crate::metadata::object([
            ("type", "response".into()),
            ("id", id),
            ("ok", JsValue::Bool(false)),
            (
                "error",
                crate::metadata::object([
                    ("code", wire_error_code(&error).into()),
                    ("message", JsValue::String(error.message().clone())),
                ]),
            ),
        ]),
    };
    stringify_json(&frame)
        .ok()
        .flatten()
        .map(String::into_bytes)
}
