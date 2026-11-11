//! A8 U8 (#1326): the six cron control methods wired into the native
//! request registry. The method bodies live in `runtime::cron` (#1325); this
//! family owns only the routing seam — the `requireCron` capability gate
//! (InProcessGateway.ts:3016-3022) and the `CronError` → `RequestError`
//! projection across GatewayWsConnection.ts:194-255.
use super::super::requests::Requests;
use crate::runtime::{
    connection::{Call, RequestError},
    cron,
};
use pilotdeck_protocol::JsValue;
use std::sync::Arc;

// `CRON_METHODS` (projection.rs:18) is the single owner of the six names —
// the same table InProcessGateway.ts:2114-2122 splices into capabilities and
// GatewayWsConnection.ts:464-475 routes. Keeping one `&str` source means the
// family cannot drift from the projection layer's dispatch slice.
pub(super) const METHODS: &[&str] = &cron::CRON_METHODS;
pub(super) const HANDLER: super::Handler = super::Handler::AfterAdmission(Requests::cron_request);

// GatewayWsConnection.ts:194-255, arm by arm, for every error the six cron
// methods can surface:
// - `SkillValidationError` / `SkillManagerError` (lines 194-222) rank above
//   everything else, but no cron path constructs them — unreachable here.
// - `DialogGatewayError` (a `CronError::gateway` carries `name:
//   "DialogGatewayError"` plus a code) or any error with a structured code
//   (lines 223-243): `{code: error.code, message: error.message}`. `details`
//   and the `sop_status` HTTP splice never apply to cron.
// - Everything else — the plain `Error`/`TypeError`/`RangeError` names —
//   falls to the last arm (lines 245-255): `{code:
//   "gateway_request_failed", message}` with the thrown value's own message.
// `wire_error_code` encodes exactly that split (`code().unwrap_or(
// "gateway_request_failed")`), and `CronError::message()` is the thrown
// value's message, so the two fields reproduce the envelope byte for byte.
fn wire_error(error: cron::CronError) -> RequestError {
    RequestError {
        code: cron::wire_error_code(&error).into(),
        message: error.message().clone(),
        details: None,
        validation: None,
    }
}

/// The blocking-closure body `cron_request` executes on the pool: the A6
/// gate and the six-method dispatch under one management read guard.
///
/// #2363 item 6: the gate check runs inside the closure with the management
/// gate's read half (the guard [`crate::switch::cron_management_gate`] hands
/// out) held across the whole body — the S1 stop face takes the write half
/// before stopping the manager, so a dispatch either completes before the
/// stop proceeds or answers the closed rejection. The pre-item-6 seam checked
/// the closed registry before the `spawn_blocking`, so a dispatch that had
/// passed the check could still run its whole body on the stopped manager
/// after S1 had already reported Ok.
///
/// `pub(crate)` so the #2363 race test runs this exact body against a stop
/// face — the same function the WS seam executes.
pub(crate) fn dispatch_under_management_gate(
    cron: &Option<Arc<crate::runtime::cron::CronManager>>,
    method: &str,
    params: &JsValue,
) -> Option<Result<JsValue, cron::CronError>> {
    // #1353 A6 (DC-5): once the switch machine's S1 has stopped the cron
    // manager, the six management methods answer an explicit rejection
    // instead of operating on the stopped manager. The TS baseline keeps
    // serving them on a stopped manager (D5) — a registered Rust difference
    // (specs/gateway-parity/diffs/). The flag is checked under the
    // management gate's read half, so a management call racing the stop
    // observes the gate, not a half-stopped manager.
    // `switch-cron-management-open` (negative control, #2363): the
    // `cron_management_closed` gate check bypassed, so the stopped manager
    // keeps serving the management methods — the TS baseline's D5 shape.
    // `switch-cron-dispatch-ungated` (negative control, #2363): the read
    // half not held across the body — the stop face's drain no longer waits
    // for this dispatch, the pre-item-6 check-then-dispatch window.
    let gate = if crate::mutants::on("switch-cron-dispatch-ungated") {
        None
    } else {
        cron.as_ref().map(crate::switch::cron_management_gate)
    };
    let admission = gate.as_ref().map(|gate| gate.enter_dispatch());
    let outcome = if crate::switch::cron_management_closed(cron.as_ref())
        && !crate::mutants::on("switch-cron-management-open")
    {
        Some(Err(cron::CronError::gateway(
            "SWITCH_IN_PROGRESS",
            pilotdeck_protocol::JsString::from("switch in progress: cron management is closed"),
        )))
    } else {
        cron::dispatch(cron.as_deref(), method, params)
    };
    // The body must finish before the read guard releases — that ordering
    // is the whole item-6 contract.
    drop(admission);
    outcome
}

impl Requests {
    pub(in crate::native_process) fn cron_request(
        &self,
        method: String,
        params: JsValue,
    ) -> Call<'_, Result<JsValue, RequestError>> {
        // #1509 P2-3b: the method bodies are synchronous and CPU-bound —
        // `cron.create` / `cron.update` run `computeNextRunAt`, whose minute
        // scan can walk the full `MAX_SEARCH_MINUTES` (366 days) before it
        // answers, and every method touches the cron store. Running that on an
        // async worker parks a whole tokio thread for the scan; TS keeps it on
        // the event loop but the port has no await inside it, so the blocking
        // pool is where it belongs. The dispatch table still owns the routing;
        // only the thread changes.
        let cron = self.cron.clone();
        let dispatch_method = method.clone();
        Box::pin(async move {
            let dispatched = tokio::task::spawn_blocking(move || {
                dispatch_under_management_gate(&cron, &dispatch_method, &params)
            })
            .await;
            match dispatched {
                Ok(Some(Ok(result))) => Ok(result),
                Ok(Some(Err(error))) => Err(wire_error(error)),
                // `dispatch` answers `None` only for names outside
                // `CRON_METHODS`; the family registry routes exactly those
                // six names here, so reaching this branch means the registry
                // and the dispatch table disagree — an implementation error
                // to report, never to silence or `unreachable!` over.
                Ok(None) => Err(RequestError::new(format!(
                    "Native cron family received unregistered method {method}."
                ))),
                // A panicking method body must surface as a request error, not
                // as a dropped connection.
                Err(join) => Err(RequestError::new(format!(
                    "Native cron method {method} failed to join: {join}"
                ))),
            }
        })
    }
}

#[cfg(test)]
mod cron_tests;
