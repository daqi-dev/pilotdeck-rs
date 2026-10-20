//! `createMcpTool` (baseline `tool/builtin/mcpTool.ts`): a single MCP tool
//! behind an injected adapter. Distinct from the runtime bridge
//! (`runtime/mcp/bridge.rs`): its own name normalisation collapses `_` runs,
//! trims edge underscores and rejects names that normalise to nothing.
use super::{object, string, tool_error};
use crate::{
    host::{HostError, modules::capability::ToolDefinition},
    runtime::tools::{ExecutableTool, ToolError, ToolFuture, ToolOutput, ToolRunContext},
};
use pilotdeck_protocol::{JsObject, JsString, JsValue};
use std::{future::Future, pin::Pin, sync::Arc};

/// Boxed adapter future.
pub type McpAdapterFuture<'a> =
    Pin<Box<dyn Future<Output = Result<JsValue, ToolError>> + Send + 'a>>;

/// `PilotDeckMcpToolAdapter`.
pub trait McpToolAdapter: Send + Sync {
    fn call_tool<'a>(
        &'a self,
        server_id: &'a str,
        tool_name: &'a str,
        input: JsValue,
    ) -> McpAdapterFuture<'a>;
}

/// `CreateMcpToolOptions`.
#[derive(Clone, Default)]
pub struct CreateMcpToolOptions {
    pub server_id: String,
    pub tool_name: String,
    pub description: Option<JsString>,
    pub input_schema: Option<JsValue>,
    pub adapter: Option<Arc<dyn McpToolAdapter>>,
}

/// `normalizeMcpName`.
fn normalize_mcp_name(value: &str) -> Result<String, ToolError> {
    let mut out = String::new();
    for unit in value.encode_utf16() {
        let keep = matches!(unit, 0x30..=0x39 | 0x41..=0x5A | 0x61..=0x7A);
        if keep {
            out.push(char::from(unit as u8));
        } else if !out.ends_with('_') {
            out.push('_');
        }
    }
    let trimmed = out.strip_prefix('_').unwrap_or(&out);
    let trimmed = trimmed.strip_suffix('_').unwrap_or(trimmed);
    if trimmed.is_empty() {
        return Err(tool_error(
            "invalid_tool_input",
            "MCP server and tool names must normalize to a name.",
        ));
    }
    Ok(trimmed.to_owned())
}

/// `buildMcpToolWireName` of `tool/builtin/mcpTool.ts`.
pub fn build_mcp_tool_wire_name(server_id: &str, tool_name: &str) -> Result<String, ToolError> {
    Ok(format!(
        "mcp__{}__{}",
        normalize_mcp_name(server_id)?,
        normalize_mcp_name(tool_name)?
    ))
}

pub struct McpTool {
    fields: JsObject,
    server_id: String,
    tool_name: String,
    wire_name: String,
    adapter: Option<Arc<dyn McpToolAdapter>>,
}

/// `createMcpTool(options)`; throws like TS when a name normalises to "".
pub fn create_mcp_tool(options: CreateMcpToolOptions) -> Result<McpTool, ToolError> {
    let wire_name = build_mcp_tool_wire_name(&options.server_id, &options.tool_name)?;
    let description = options.description.unwrap_or_else(|| {
        format!("Call MCP tool {}/{}.", options.server_id, options.tool_name).into()
    });
    let input_schema = options.input_schema.unwrap_or_else(|| {
        JsValue::Object(object([
            ("type", string("object")),
            ("additionalProperties", JsValue::Bool(true)),
            ("properties", JsValue::Object(JsObject::new())),
        ]))
    });
    Ok(McpTool {
        fields: object([
            ("name", string(wire_name.as_str())),
            ("description", JsValue::String(description)),
            ("kind", string("mcp")),
            ("inputSchema", input_schema),
            ("maxResultBytes", JsValue::Number(200_000.0)),
        ]),
        server_id: options.server_id,
        tool_name: options.tool_name,
        wire_name,
        adapter: options.adapter,
    })
}

impl McpTool {
    /// `isOpenWorld()`.
    pub fn is_open_world(&self) -> bool {
        true
    }
}

impl ToolDefinition for McpTool {
    fn fields(&self) -> &JsObject {
        &self.fields
    }
    fn is_read_only(&self, _input: &JsValue) -> Result<JsValue, HostError> {
        Ok(JsValue::Bool(true))
    }
    fn is_concurrency_safe(&self, _input: &JsValue) -> Result<JsValue, HostError> {
        Ok(JsValue::Bool(false))
    }
}

impl ExecutableTool for McpTool {
    fn execute(&self, input: JsValue, _context: ToolRunContext) -> ToolFuture<ToolOutput> {
        let adapter = self.adapter.clone();
        let (server_id, tool_name, wire_name) = (
            self.server_id.clone(),
            self.tool_name.clone(),
            self.wire_name.clone(),
        );
        Box::pin(async move {
            let Some(adapter) = adapter else {
                return Err(tool_error(
                    "unsupported_tool",
                    "MCP adapter is not configured.",
                ));
            };
            let value = adapter.call_tool(&server_id, &tool_name, input).await?;
            Ok(ToolOutput {
                content: vec![JsValue::Object(object([
                    ("type", string("json")),
                    ("value", value.clone()),
                ]))],
                supplemental_messages: None,
                data: Some(value),
                metadata: Some(object([(
                    "mcp",
                    JsValue::Object(object([
                        ("serverId", string(server_id.as_str())),
                        ("toolName", string(tool_name.as_str())),
                        ("wireName", string(wire_name.as_str())),
                    ])),
                )])),
            })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_normalise_like_the_tool_builtin() {
        assert_eq!(
            build_mcp_tool_wire_name("my server", "a..b").unwrap(),
            "mcp__my_server__a_b"
        );
        assert_eq!(
            build_mcp_tool_wire_name("_x_", "__y__").unwrap(),
            "mcp__x__y"
        );
        assert_eq!(
            build_mcp_tool_wire_name("a-b", "工具t").unwrap(),
            "mcp__a_b__t"
        );
        assert!(build_mcp_tool_wire_name("!!", "t").is_err());
    }
}
