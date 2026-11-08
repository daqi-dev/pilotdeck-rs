//! Structured terminal payloads for CLI SDK and machine consumers.
use super::{object, schema, string};
use crate::{
    host::{HostError, modules::capability::ToolDefinition},
    runtime::tools::{ExecutableTool, ToolFuture, ToolOutput, ToolRunContext},
};
use pilotdeck_protocol::{JsObject, JsValue};

pub struct StructuredOutputTool { fields: JsObject }
impl Default for StructuredOutputTool {
    fn default() -> Self {
        Self { fields: object([
            ("name", string("structured_output")),
            ("aliases", JsValue::Array(vec![string("StructuredOutput")])),
            ("description", string("Return a final structured output payload for non-interactive hosts.")),
            ("kind", string("structured_output")),
            ("inputSchema", JsValue::Object(schema(r#"{"type":"object","required":["value"],"additionalProperties":false,"properties":{"value":{"type":["object","array","string","number","boolean"],"description":"The structured payload to return (any JSON-serializable value)."}}}"#))),
        ]) }
    }
}
impl ToolDefinition for StructuredOutputTool {
    fn fields(&self) -> &JsObject { &self.fields }
    fn is_read_only(&self, _: &JsValue) -> Result<JsValue, HostError> { Ok(JsValue::Bool(true)) }
    fn is_concurrency_safe(&self, _: &JsValue) -> Result<JsValue, HostError> { Ok(JsValue::Bool(true)) }
}
impl ExecutableTool for StructuredOutputTool {
    fn execute(&self, input: JsValue, _: ToolRunContext) -> ToolFuture<ToolOutput> {
        Box::pin(async move { Ok(output(input.get("value").cloned().unwrap_or(JsValue::Undefined))) })
    }
}
fn output(value: JsValue) -> ToolOutput {
    ToolOutput {
        content: vec![JsValue::Object(object([("type", string("json")), ("value", value.clone())]))],
        data: Some(value),
        metadata: Some(object([("structuredOutput", JsValue::Bool(true))])),
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn typed_payload_keeps_false_zero_and_nested_values() {
        for json in ["false", "0", "\"\"", "[]", r#"{"nested":[null,false,0,{"x":"y"}]}"#] {
            let value = pilotdeck_protocol::parse_json(json).unwrap();
            let result = output(value.clone());
            assert_eq!(result.data, Some(value.clone()));
            assert_eq!(result.content[0].get("value"), Some(&value));
            assert_eq!(result.metadata.unwrap().get("structuredOutput"), Some(&JsValue::Bool(true)));
        }
    }
}
