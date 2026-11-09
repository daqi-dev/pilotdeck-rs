//! Empty builtin for tool-call latency baselines; no side effects.
use super::{object, string};
use crate::{
    host::{HostError, modules::capability::ToolDefinition},
    runtime::tools::{ExecutableTool, ToolFuture, ToolOutput, ToolRunContext},
};
use pilotdeck_protocol::{JsObject, JsValue};

pub struct NoopTool {
    fields: JsObject,
}

impl NoopTool {
    pub fn new() -> Self {
        Self {
            fields: definition(),
        }
    }
}

impl Default for NoopTool {
    fn default() -> Self {
        Self::new()
    }
}

impl ToolDefinition for NoopTool {
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

impl ExecutableTool for NoopTool {
    fn execute(&self, _input: JsValue, _context: ToolRunContext) -> ToolFuture<ToolOutput> {
        Box::pin(async move {
            Ok(ToolOutput {
                content: vec![JsValue::Object(object([
                    ("type", "text".into()),
                    ("text", string("ok")),
                ]))],
                supplemental_messages: None,
                data: None,
                metadata: None,
            })
        })
    }
}

fn definition() -> JsObject {
    object([
        ("name", string("noop")),
        ("title", string("No-op")),
        (
            "description",
            string("Performance baseline only: returns immediately with no side effects."),
        ),
        ("kind", string("custom")),
        (
            "inputSchema",
            JsValue::Object(super::schema(
                r#"{"type":"object","additionalProperties":false,"properties":{}}"#,
            )),
        ),
    ])
}
