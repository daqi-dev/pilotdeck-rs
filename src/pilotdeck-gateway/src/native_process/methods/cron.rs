//! A8 U8 (#1326): the six cron control methods wired into the native
//! request registry. The method bodies live in `runtime::cron` (#1325); this
//! family owns only the routing seam — the `requireCron` capability gate
//! (InProcessGateway.ts:3016-3022) and the `CronError` → `RequestError`
//! projection across GatewayWsConnection.ts:215-255.
use super::super::requests::Requests;
use crate::runtime::{
    connection::{Call, RequestError},
    cron,
};
use pilotdeck_protocol::JsValue;

// `CRON_METHODS` (projection.rs:18) is the single owner of the six names —
// the same table InProcessGateway.ts:2114-2122 splices into capabilities and
// GatewayWsConnection.ts:464-470 routes. Keeping one `&str` source means the
// family cannot drift from the projection layer's dispatch slice.
pub(super) const METHODS: &[&str] = &cron::CRON_METHODS;
pub(super) const HANDLER: super::Handler = super::Handler::AfterAdmission(Requests::cron_request);

// GatewayWsConnection.ts:215-255, arm by arm, for every error the six cron
// methods can surface:
// - `SkillValidationError` / `SkillManagerError` (lines 199-218) rank above
//   everything else, but no cron path constructs them — unreachable here.
// - `DialogGatewayError` (a `CronError::gateway` carries `name:
//   "DialogGatewayError"` plus a code) or any error with a structured code
//   (lines 219-252): `{code: error.code, message: error.message}`. `details`
//   and the `sop_status` HTTP splice never apply to cron.
// - Everything else — the plain `Error`/`TypeError`/`RangeError` names —
//   falls to the last arm (lines 253-260): `{code:
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
        let cron = self.current_cron();
        let dispatch_method = method.clone();
        Box::pin(async move {
            // #1353 A6 (DC-5): once the switch machine's S1 has stopped the
            // cron manager, the six management methods answer an explicit
            // rejection instead of operating on the stopped manager. The TS
            // baseline keeps serving them on a stopped manager (D5) — a
            // registered Rust difference (specs/gateway-parity/diffs/). The
            // flag is checked before the dispatch leaves the routing seam,
            // so a management call racing the stop observes the gate, not a
            // half-stopped manager.
            if crate::switch::cron_management_closed(cron.as_ref()) {
                return Err(wire_error(cron::CronError::gateway(
                    "SWITCH_IN_PROGRESS",
                    pilotdeck_protocol::JsString::from(
                        "switch in progress: cron management is closed",
                    ),
                )));
            }
            let dispatched = tokio::task::spawn_blocking(move || {
                cron::dispatch(cron.as_deref(), &dispatch_method, &params)
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
