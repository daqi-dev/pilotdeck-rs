//! U12 `prepare_weixin_login` — the weixin login bootstrap RPC (#1328).
//!
//! TS references:
//!  - `InProcessGateway.ts:2912-2921` — the gateway-level guard: no callback
//!    configured → `{requested: false, requestedAt: <now ISO>, reason:
//!    "unsupported"}`; otherwise the callback's result is forwarded as-is.
//!  - `GatewayWsConnection.ts:528-536` — the wire dispatch case with the same
//!    unsupported fallback, handled at the connection switch before generic
//!    request routing (like `reload_config`), ignoring `frame.params`.
//!  - `pilotdeck.ts:240-247` — the CLI callback: `requestedAt` is captured
//!    before the await, `{requested: true, requestedAt}` after the hot start.
//!
//! Connection-owned placement in TS means the method answers independent of
//! session/project admission, so the Rust family registers BeforeAdmission
//! and ignores `params`. The callback itself lands with the A8.3 assembly
//! wiring; until then the field is `None` and the method answers the
//! unsupported shape instead of `metadata::unavailable`.
use super::super::requests::Requests;
use crate::{
    metadata::object,
    runtime::connection::RequestError,
};
use pilotdeck_protocol::JsValue;
use pilotdeck_transport::{server::ExecutionClock, system_clock::SystemExecutionClock};
use std::{future::Future, pin::Pin, sync::Arc};

pub(super) const METHODS: &[&str] = &["prepare_weixin_login"];
pub(super) const HANDLER: super::Handler = super::Handler::BeforeAdmission(
    |requests, _method, _params| Box::pin(requests.prepare_weixin_login_request()),
);

/// The `InProcessGatewayOptions.prepareWeixinLogin` callback. A rejection
/// surfaces as the RPC error, like an awaited TS rejection does.
pub type PrepareWeixinLoginFuture =
    Pin<Box<dyn Future<Output = Result<JsValue, RequestError>> + Send>>;
pub type PrepareWeixinLogin = Arc<dyn Fn() -> PrepareWeixinLoginFuture + Send + Sync>;

impl Requests {
    pub(in crate::native_process) async fn prepare_weixin_login_request(
        &self,
    ) -> Option<Result<JsValue, RequestError>> {
        Some(match &self.prepare_weixin_login {
            None => Ok(object([
                ("requested", JsValue::Bool(false)),
                (
                    "requestedAt",
                    // `new Date().toISOString()` at the moment of the call.
                    JsValue::String(SystemExecutionClock.now_iso()),
                ),
                ("reason", "unsupported".into()),
            ])),
            Some(prepare) => prepare().await,
        })
    }
}
