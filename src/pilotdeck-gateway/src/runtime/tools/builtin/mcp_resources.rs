//! `list_mcp_resources` / `read_mcp_resource` (baseline
//! `tool/builtin/mcpResources.ts`) behind an injected resource adapter. The
//! fixed baseline wires no adapter into its runtime (`McpClient` has no
//! resources methods), so these tools report `unsupported_tool` unless the
//! host supplies one.
use super::{object, string, tool_error};
use crate::{
    host::{HostError, modules::capability::ToolDefinition},
    runtime::tools::{ExecutableTool, ToolError, ToolFuture, ToolOutput, ToolRunContext},
};
use pilotdeck_protocol::{JsObject, JsValue};
use std::{future::Future, pin::Pin, sync::Arc};

pub type McpResourceFuture<'a> =
    Pin<Box<dyn Future<Output = Result<JsValue, ToolError>> + Send + 'a>>;

/// `PilotDeckMcpResourceAdapter`; arguments are the raw input fields.
pub trait McpResourceAdapter: Send + Sync {
    fn list_resources(&self, server_id: JsValue) -> McpResourceFuture<'_>;
    fn read_resource(&self, server_id: JsValue, uri: JsValue) -> McpResourceFuture<'_>;
}

#[derive(Clone, Copy)]
enum Kind {
    List,
    Read,
}

pub struct McpResourceTool {
    fields: JsObject,
    kind: Kind,
    adapter: Option<Arc<dyn McpResourceAdapter>>,
}

fn field(input: &JsValue, key: &str) -> JsValue {
    input.get(key).cloned().unwrap_or(JsValue::Undefined)
}

/// `createListMcpResourcesTool(adapter)`.
pub fn create_list_mcp_resources_tool(
    adapter: Option<Arc<dyn McpResourceAdapter>>,
) -> McpResourceTool {
    McpResourceTool {
        fields: object([
            ("name", string("list_mcp_resources")),
            (
                "aliases",
                JsValue::Array(vec![string("ListMcpResourcesTool")]),
            ),
            (
                "description",
                string("List resources exposed by configured MCP servers."),
            ),
            ("kind", string("mcp")),
            (
                "inputSchema",
                JsValue::Object(super::schema(
                    r#"{"type":"object","additionalProperties":false,"properties":{"serverId":{"type":"string","description":"Optional MCP server id to filter resources. Omit to list all servers."}}}"#,
                )),
            ),
        ]),
        kind: Kind::List,
        adapter,
    }
}

/// `createReadMcpResourceTool(adapter)`.
pub fn create_read_mcp_resource_tool(
    adapter: Option<Arc<dyn McpResourceAdapter>>,
) -> McpResourceTool {
    McpResourceTool {
        fields: object([
            ("name", string("read_mcp_resource")),
            (
                "aliases",
                JsValue::Array(vec![string("ReadMcpResourceTool")]),
            ),
            (
                "description",
                string("Read a resource exposed by a configured MCP server."),
            ),
            ("kind", string("mcp")),
            (
                "inputSchema",
                JsValue::Object(super::schema(
                    r#"{"type":"object","required":["serverId","uri"],"additionalProperties":false,"properties":{"serverId":{"type":"string","description":"MCP server identifier that hosts the resource."},"uri":{"type":"string","description":"Resource URI to read (as listed by list_mcp_resources)."}}}"#,
                )),
            ),
            ("maxResultBytes", JsValue::Number(200_000.0)),
        ]),
        kind: Kind::Read,
        adapter,
    }
}

impl McpResourceTool {
    /// `isOpenWorld()`.
    pub fn is_open_world(&self) -> bool {
        true
    }
}

impl ToolDefinition for McpResourceTool {
    fn fields(&self) -> &JsObject {
        &self.fields
    }
    fn is_read_only(&self, _input: &JsValue) -> Result<JsValue, HostError> {
        Ok(JsValue::Bool(true))
    }
    fn is_concurrency_safe(&self, _input: &JsValue) -> Result<JsValue, HostError> {
        Ok(JsValue::Bool(true))
    }
}

impl ExecutableTool for McpResourceTool {
    fn execute(&self, input: JsValue, _context: ToolRunContext) -> ToolFuture<ToolOutput> {
        let adapter = self.adapter.clone();
        let kind = self.kind;
        Box::pin(async move {
            let Some(adapter) = adapter else {
                return Err(tool_error(
                    "unsupported_tool",
                    "MCP resource adapter is not configured.",
                ));
            };
            let value = match kind {
                Kind::List => adapter.list_resources(field(&input, "serverId")).await?,
                Kind::Read => {
                    adapter
                        .read_resource(field(&input, "serverId"), field(&input, "uri"))
                        .await?
                }
            };
            Ok(ToolOutput {
                content: vec![JsValue::Object(object([
                    ("type", string("json")),
                    ("value", value.clone()),
                ]))],
                supplemental_messages: None,
                data: Some(value),
                metadata: None,
            })
        })
    }
}
