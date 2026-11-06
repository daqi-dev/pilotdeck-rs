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
        Box::pin(async move {
            match cron::dispatch(self.cron.as_deref(), &method, &params) {
                Some(Ok(result)) => Ok(result),
                Some(Err(error)) => Err(wire_error(error)),
                // `dispatch` answers `None` only for names outside
                // `CRON_METHODS`; the family registry routes exactly those
                // six names here, so reaching this branch means the registry
                // and the dispatch table disagree — an implementation error
                // to report, never to silence or `unreachable!` over.
                None => Err(RequestError::new(format!(
                    "Native cron family received unregistered method {method}."
                ))),
            }
        })
    }
}

#[cfg(test)]
mod cron_tests;
