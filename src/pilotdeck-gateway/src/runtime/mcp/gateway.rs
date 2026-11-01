//! U6 MCP control gateway adapter (#1215). Mirrors the TS gateway layers the
//! four SDK MCP control methods cross: `GatewayWsConnection.ts` dispatch
//! (`set_mcp_servers` / `mcp_server_reconnect` / `mcp_server_toggle` /
//! `set_mcp_permission_mode_override`), then `InProcessGateway.ts:2284-2319`
//! (field validation, the per-key `hasActiveTurn` SESSION_BUSY gate, the
//! registry callback, and `router.close(sessionKey)` after success, with the
//! close error propagating), then the registry `*ForSdk` methods already
//! mirrored by [`super::sdk::SdkMcpConfig`].
//!
//! This is a component, not a wiring change: `dispatch.rs` and the default
//! routing table are untouched. The session side only uses the #1011
//! `SessionOwner` admission/close ports; `runtime/session` itself is not
//! modified. A caller assembling this component answers for the capability
//! gate: the TS gateway assembled without the four options answers
//! `CAPABILITY_UNAVAILABLE` per method (recorded in the golden's
//! `assemblyCases`), so any future wiring must check the capability before
//! routing to this adapter instead of mounting it unconditionally.
use super::sdk::{SdkMcpConfig, SdkMcpError, range_error, require_input, required_trimmed};
use crate::js_value;
use crate::runtime::session::SessionOwner;
use pilotdeck_protocol::{JsObject, JsString, JsValue};
use std::sync::Arc;

pub struct McpGatewayOptions {
    /// The #1011 router-facing admission/close port. `has_active_turn` drives
    /// the SESSION_BUSY gate and `close` is the post-success session close.
    pub owner: SessionOwner,
    /// The registry layer (`ProjectRuntimeRegistry`'s `*ForSdk` methods).
    pub config: Arc<SdkMcpConfig>,
}

/// The four SDK MCP controls with `InProcessGateway`'s method-level contract.
///
/// Not part of the default routing: callers assemble this component when they
/// want the methods; nothing in `dispatch.rs` references it yet.
pub struct McpGateway {
    options: McpGatewayOptions,
}

impl McpGateway {
    pub fn new(options: McpGatewayOptions) -> Self {
        Self { options }
    }

    /// `InProcessGateway.setMcpServers` (client/InProcessGateway.ts:2284).
    pub async fn set_mcp_servers(&self, input: &JsValue) -> Result<JsValue, SdkMcpError> {
        let input = require_input(input, "sessionKey")?;
        let session_key = required_trimmed(
            input.get("sessionKey"),
            "sessionKey",
            "INVALID_SESSION_KEY",
            "sessionKey is required.",
        )?;
        self.reject_active_turn(
            &session_key,
            "Cannot change MCP servers while a turn is active.",
        )?;
        let result = self.options.config.set_mcp_servers(input).await?;
        self.close(&session_key).await?;
        Ok(result)
    }

    /// `InProcessGateway.reconnectMcpServer` (client/InProcessGateway.ts:2293).
    pub async fn reconnect_mcp_server(&self, input: &JsValue) -> Result<(), SdkMcpError> {
        let input = require_input(input, "sessionKey")?;
        let session_key = self.control_input(input)?;
        self.reject_active_turn(&session_key, "Cannot reconnect MCP while a turn is active.")?;
        self.options.config.reconnect_mcp_server(input).await?;
        self.close(&session_key).await
    }

    /// `InProcessGateway.toggleMcpServer` (client/InProcessGateway.ts:2301).
    pub async fn toggle_mcp_server(&self, input: &JsValue) -> Result<(), SdkMcpError> {
        let input = require_input(input, "sessionKey")?;
        let session_key = self.control_input(input)?;
        self.reject_active_turn(&session_key, "Cannot toggle MCP while a turn is active.")?;
        self.options.config.toggle_mcp_server(input).await?;
        self.close(&session_key).await
    }

    /// `InProcessGateway.setMcpPermissionModeOverride`
    /// (client/InProcessGateway.ts:2309). The mode check precedes the busy
    /// gate, exactly like the baseline's `if` ordering.
    pub async fn set_mcp_permission_mode_override(
        &self,
        input: &JsValue,
    ) -> Result<JsValue, SdkMcpError> {
        let input = require_input(input, "sessionKey")?;
        let session_key = self.control_input(input)?;
        let mode = input.get("mode").cloned().unwrap_or(JsValue::Undefined);
        let mode_ok = match &mode {
            JsValue::Null => true,
            JsValue::String(value) => value.equals_str("default") || value.equals_str("auto"),
            _ => false,
        };
        if !mode_ok {
            let mut units = "Unsupported MCP permission mode: "
                .encode_utf16()
                .collect::<Vec<_>>();
            units.extend(js_value::to_string_units(&mode).map_err(range_error)?);
            return Err(SdkMcpError::gateway(
                "INVALID_MCP_PERMISSION_MODE",
                JsString::from_utf16(units),
            ));
        }
        self.reject_active_turn(
            &session_key,
            "Cannot change MCP permission mode while a turn is active.",
        )?;
        let result = self
            .options
            .config
            .set_mcp_permission_mode_override(input)
            .await?;
        self.close(&session_key).await?;
        Ok(result)
    }

    /// The `GatewayWsConnection` dispatch entry for the four methods
    /// (server/GatewayWsConnection.ts:387-398): `None` means the method is
    /// not one of the four and belongs to the caller's wider dispatch;
    /// `mcp_server_reconnect` / `mcp_server_toggle` answer `{ ok: true }`
    /// because the WS layer wraps their `Promise<void>`.
    pub async fn dispatch(
        &self,
        method: &str,
        input: &JsValue,
    ) -> Option<Result<JsValue, SdkMcpError>> {
        let result = match method {
            "set_mcp_servers" => self.set_mcp_servers(input).await,
            "mcp_server_reconnect" => self.reconnect_mcp_server(input).await.map(|()| ok_true()),
            "mcp_server_toggle" => self.toggle_mcp_server(input).await.map(|()| ok_true()),
            "set_mcp_permission_mode_override" => {
                self.set_mcp_permission_mode_override(input).await
            }
            _ => return None,
        };
        Some(result)
    }

    /// The shared `!input.sessionKey?.trim() || !input.serverName?.trim()`
    /// validation of the three per-server controls. The left operand is
    /// evaluated first, so a non-string `sessionKey` throws before
    /// `serverName` is ever read, as in JS. Returns the raw session key.
    fn control_input(&self, input: &JsValue) -> Result<JsString, SdkMcpError> {
        let session_key = required_trimmed(
            input.get("sessionKey"),
            "sessionKey",
            "INVALID_MCP_SERVER",
            "sessionKey and serverName are required.",
        )?;
        required_trimmed(
            input.get("serverName"),
            "serverName",
            "INVALID_MCP_SERVER",
            "sessionKey and serverName are required.",
        )?;
        Ok(session_key)
    }

    /// `if (this.router.hasActiveTurn(input.sessionKey))` — per session key,
    /// before the registry callback, so a busy session neither resolves a
    /// project nor closes.
    fn reject_active_turn(
        &self,
        session_key: &JsString,
        message: &'static str,
    ) -> Result<(), SdkMcpError> {
        if self.options.owner.has_active_turn(session_key) {
            Err(SdkMcpError::gateway("SESSION_BUSY", message))
        } else {
            Ok(())
        }
    }

    /// `await this.router.close(input.sessionKey)` after a successful callback.
    /// A close failure propagates to the caller instead of being swallowed.
    /// The WS error frame carries only code+message, so the TS `AggregateError`
    /// name of a dispose failure is intentionally flattened to `"Error"` here;
    /// the name is not part of the golden contract.
    async fn close(&self, session_key: &JsString) -> Result<(), SdkMcpError> {
        self.options
            .owner
            .close(session_key)
            .await
            .map_err(|error| SdkMcpError::plain("Error", JsString::from(error.message)))
    }
}

/// `.then(() => ({ ok: true }))` from the WS dispatch of the two void
/// controls.
fn ok_true() -> JsValue {
    let mut object = JsObject::new();
    object.insert("ok".into(), JsValue::Bool(true));
    JsValue::Object(object)
}
