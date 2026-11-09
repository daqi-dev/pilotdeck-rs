//! Final structured output payload for non-interactive hosts.
use super::{object, schema, string};
use crate::{
    host::{HostError, modules::capability::ToolDefinition},
    runtime::tools::{ExecutableTool, ToolError, ToolFuture, ToolOutput, ToolRunContext},
};
use pilotdeck_protocol::{JsObject, JsValue};

pub struct StructuredOutputTool {
    fields: JsObject,
}
impl StructuredOutputTool {
    pub fn new() -> Self {
        Self {
            fields: object([
                ("name", string("structured_output")),
                ("aliases", JsValue::Array(vec![string("StructuredOutput")])),
                (
                    "description",
                    string("Return a final structured output payload for non-interactive hosts."),
                ),
                ("kind", string("structured_output")),
                (
                    "inputSchema",
                    JsValue::Object(schema(
                        r#"{"type":"object","required":["value"],"additionalProperties":false,"properties":{"value":{"type":["object","array","string","number","boolean"]}}}"#,
                    )),
                ),
            ]),
        }
    }
}
impl Default for StructuredOutputTool {
    fn default() -> Self {
        Self::new()
    }
}
impl ToolDefinition for StructuredOutputTool {
    fn fields(&self) -> &JsObject {
        &self.fields
    }
    fn is_read_only(&self, _: &JsValue) -> Result<JsValue, HostError> {
        Ok(JsValue::Bool(true))
    }
    fn is_concurrency_safe(&self, _: &JsValue) -> Result<JsValue, HostError> {
        Ok(JsValue::Bool(true))
    }
}
impl ExecutableTool for StructuredOutputTool {
    fn execute(&self, input: JsValue, _: ToolRunContext) -> ToolFuture<ToolOutput> {
        Box::pin(async move {
            let value = input.get("value").cloned().ok_or_else(|| {
                ToolError::new("invalid_tool_input", "structured_output requires value.")
            })?;
            if matches!(value, JsValue::Null | JsValue::Undefined) {
                return Err(ToolError::new(
                    "invalid_tool_input",
                    "structured_output value must be non-null JSON.",
                ));
            }
            Ok(ToolOutput {
                content: vec![JsValue::Object(object([
                    ("type", string("json")),
                    ("value", value.clone()),
                ]))],
                data: Some(value),
                metadata: Some(object([("structuredOutput", JsValue::Bool(true))])),
                ..Default::default()
            })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host::modules::tool_context::{HostToolCheckpoint, ToolRuntimeContext};
    use std::sync::Arc;
    fn context() -> ToolRunContext {
        ToolRunContext::new(Arc::new(ToolRuntimeContext {
            fields: JsObject::new(),
            files: HostToolCheckpoint::default().tool_context_state(),
            abort_signal: None,
            model: None,
            elicitation: None,
            user_dialog: None,
            audit_recorder: None,
            file_history: None,
            file_update_notifier: None,
            now: None,
            progress: None,
            subagent: None,
            plan_todo: None,
            goal: None,
            plan_directory: None,
        }))
    }
    #[tokio::test]
    async fn returns_value_and_rejects_null() {
        let tool = StructuredOutputTool::new();
        let output = tool
            .execute(
                JsValue::Object(object([("value", JsValue::Bool(true))])),
                context(),
            )
            .await
            .unwrap();
        assert_eq!(output.data, Some(JsValue::Bool(true)));
        assert!(
            tool.execute(
                JsValue::Object(object([("value", JsValue::Null)])),
                context()
            )
            .await
            .is_err()
        );
    }
}
