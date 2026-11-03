//! U6 MCP control methods wired into the native request router (#1319).
//! TS baseline: `InProcessGateway.ts:2284-2320` (the four SDK MCP control
//! methods), `GatewayWsConnection.ts:387-398` (WS dispatch).
//!
//! The `McpGateway` component (#1215) owns field validation, `hasActiveTurn`
//! rejection, the registry callback, and `router.close(sessionKey)` after
//! success. This family is the thin adapter that routes the four wire names
//! into that component and maps `SdkMcpError` to `RequestError`.
use super::super::requests::Requests;
use crate::runtime::{
    connection::{Call, RequestError},
    mcp::sdk::SdkMcpError,
};
use pilotdeck_protocol::JsValue;

pub(super) const METHODS: &[&str] = &[
    "set_mcp_servers",
    "mcp_server_reconnect",
    "mcp_server_toggle",
    "set_mcp_permission_mode_override",
];

pub(super) const HANDLER: super::Handler = super::Handler::AfterAdmission(Requests::mcp_request);

fn mcp_error(e: SdkMcpError) -> RequestError {
    RequestError {
        code: e.code().unwrap_or("gateway_request_failed").into(),
        message: e.message().clone(),
        details: None,
        validation: None,
    }
}

/// `{ ok: true }` for the two void controls — mirrors the WS-layer wrapper
/// that `McpGateway::dispatch` produces for `mcp_server_reconnect` /
/// `mcp_server_toggle` (`{ ok: true }`).
fn ok_true() -> JsValue {
    use pilotdeck_protocol::JsObject;
    let mut object = JsObject::new();
    object.insert("ok".into(), JsValue::Bool(true));
    JsValue::Object(object)
}

impl Requests {
    pub(in crate::native_process) fn mcp_request(
        &self,
        method: String,
        params: JsValue,
    ) -> Call<'_, Result<JsValue, RequestError>> {
        Box::pin(async move {
            let gateway = &self.mcp;
            match method.as_str() {
                "set_mcp_servers" => gateway.set_mcp_servers(&params).await.map_err(mcp_error),
                "mcp_server_reconnect" => gateway
                    .reconnect_mcp_server(&params)
                    .await
                    .map(|()| ok_true())
                    .map_err(mcp_error),
                "mcp_server_toggle" => gateway
                    .toggle_mcp_server(&params)
                    .await
                    .map(|()| ok_true())
                    .map_err(mcp_error),
                "set_mcp_permission_mode_override" => gateway
                    .set_mcp_permission_mode_override(&params)
                    .await
                    .map_err(mcp_error),
                _ => super::metadata::unavailable(method),
            }
        })
    }
}
