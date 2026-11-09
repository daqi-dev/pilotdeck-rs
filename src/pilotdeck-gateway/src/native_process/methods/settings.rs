//! U7 global settings methods wired into the native request router (#1320).
//! TS baseline: `GatewayWsConnection.ts:405-425, 523-543` (the WS dispatch
//! of the six methods), `InProcessGateway.ts:2337-2362, 2387-2390,
//! 2905-2928` (the wrapper contracts).
//!
//! The `SettingsGateway` component (#1318) owns the method-level contracts:
//! the per-method capability gates (the four rejecting methods throw
//! `CAPABILITY_UNAVAILABLE` `"<method> is unavailable."`; the two reloads
//! project `{reloaded:false, reason:"unsupported"}`), the field validation,
//! the `hasActiveTurn` SESSION_BUSY gate, the post-success session close and
//! the createLocalGateway `updateSettings` closure. This family is the thin
//! adapter that routes the six wire names into that component and maps
//! `SettingsError` to the WS error frame's `RequestError` projection
//! (`code: None` is a raw JS throw, which the WS layer answers
//! `gateway_request_failed` for, like the MCP slice's adapter).
//!
//! Production binds the shared session flag store, atomic local-settings writer,
//! resolved config snapshot and refresh bundle. Absent optional delegates retain
//! the component's explicit capability-unavailable response.
use super::super::requests::Requests;
use crate::runtime::{
    connection::{Call, RequestError},
    settings::SettingsError,
};
use pilotdeck_protocol::{JsString, JsValue};

// The single source is the component's list (`runtime/settings/gateway.rs`);
// #1320 fix-r2 F10: re-export instead of hand-copying, so the registry table
// and the dispatch arms cannot drift apart.
pub(crate) const METHODS: &[&str] = crate::runtime::settings::METHODS;

pub(super) const HANDLER: super::Handler =
    super::Handler::AfterAdmission(Requests::settings_request);

fn settings_error(error: SettingsError) -> RequestError {
    RequestError {
        code: error
            .code()
            .map(|code| String::from_utf16_lossy(code.units()))
            .unwrap_or_else(|| "gateway_request_failed".into()),
        message: error.message().clone(),
        details: None,
        validation: None,
    }
}

impl Requests {
    pub(in crate::native_process) fn settings_request(
        &self,
        method: String,
        params: JsValue,
    ) -> Call<'_, Result<JsValue, RequestError>> {
        Box::pin(async move {
            let gateway = &self.settings;
            match gateway.dispatch(&method, &params).await {
                Some(result) => result.map_err(settings_error),
                // The registry selected this family, so the wire name is one
                // of the six the component dispatches; if the two lists ever
                // drift apart, answer the caller with the WS layer's own
                // unknown-method projection instead of panicking the process
                // (#1320 fix-r1 F5).
                None => Err(RequestError {
                    code: "gateway_request_failed".into(),
                    message: JsString::from(format!("unknown settings method: {method}")),
                    details: None,
                    validation: None,
                }),
            }
        })
    }
}
