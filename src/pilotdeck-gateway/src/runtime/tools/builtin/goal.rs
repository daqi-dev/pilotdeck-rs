//! Session goal tools; all writes use the existing durable session provider.
use super::{object, schema, string};
use crate::{
    host::{HostError, modules::capability::ToolDefinition},
    runtime::{
        goal::GoalSession,
        tools::{ExecutableTool, ToolError, ToolFuture, ToolOutput, ToolRunContext},
    },
};
use pilotdeck_protocol::{JsObject, JsValue};
use std::sync::Arc;
#[derive(Clone, Copy)]
enum Kind {
    Get,
    Create,
    Update,
}
struct GoalTool {
    fields: JsObject,
    kind: Kind,
}
pub fn tools() -> Vec<Arc<dyn ExecutableTool>> {
    [Kind::Get,Kind::Create,Kind::Update].into_iter().map(|kind| {
        let (name, description, input) = match kind {
            Kind::Get => ("get_goal", "Read the current durable goal for this session.", r#"{"type":"object","properties":{},"additionalProperties":false}"#),
            Kind::Create => ("create_goal", "Create the session's durable goal. Only one goal can exist at a time.", r#"{"type":"object","properties":{"objective":{"type":"string","description":"The outcome this session is pursuing."},"max_goal_rounds":{"type":"number","description":"Optional maximum number of continuation rounds."},"id":{"type":"string","description":"Optional stable goal identifier."}},"required":["objective"],"additionalProperties":false}"#),
            Kind::Update => ("update_goal", "Edit or transition the session's durable goal using an optional expected revision for CAS safety.", r#"{"type":"object","properties":{"action":{"type":"string","enum":["edit","pause","resume","complete","block","clear"]},"expected_revision":{"type":"number"},"objective":{"type":"string"},"blocked_reason":{"type":"string"},"max_goal_rounds":{"type":"number"}},"additionalProperties":false}"#),
        };
        Arc::new(GoalTool {kind, fields: object([("name",string(name)),("description",string(description)),("kind",string("session")),("inputSchema",JsValue::Object(schema(input)))])}) as Arc<dyn ExecutableTool>
    }).collect()
}
impl ToolDefinition for GoalTool {
    fn fields(&self) -> &JsObject {
        &self.fields
    }
    fn is_read_only(&self, input: &JsValue) -> Result<JsValue, HostError> {
        Ok(JsValue::Bool(match self.kind {
            Kind::Get => true,
            Kind::Create => false,
            Kind::Update => {
                input.get("action") == Some(&"edit".into())
                    && ["objective", "blocked_reason", "max_goal_rounds"]
                        .iter()
                        .all(|key| input.get(key).is_none_or(JsValue::is_undefined))
            }
        }))
    }
    fn is_concurrency_safe(&self, _: &JsValue) -> Result<JsValue, HostError> {
        Ok(JsValue::Bool(matches!(self.kind, Kind::Get)))
    }
}
impl ExecutableTool for GoalTool {
    fn execute(&self, input: JsValue, context: ToolRunContext) -> ToolFuture<ToolOutput> {
        let kind = self.kind;
        Box::pin(async move {
            let goal = context
                .host
                .goal
                .clone()
                .and_then(|value| value.downcast::<GoalSession>().ok())
                .ok_or_else(|| {
                    ToolError::new(
                        "unsupported_tool",
                        "Goal tools require a session-scoped goal provider.",
                    )
                })?;
            let (value, operation) = match kind {
                Kind::Get => (goal.snapshot()?.goal, "get"),
                Kind::Create | Kind::Update => {
                    if context.is_aborted() {
                        return Err(ToolError::aborted());
                    }
                    let turn = context
                        .fields
                        .get("turnId")
                        .and_then(JsValue::as_string)
                        .cloned()
                        .unwrap_or_else(|| "goal".into());
                    (
                        goal.mutate(matches!(kind, Kind::Create), input, turn)
                            .await?,
                        if matches!(kind, Kind::Create) {
                            "create"
                        } else {
                            "update"
                        },
                    )
                }
            };
            let revision = value.get("revision").cloned().unwrap_or(JsValue::Null);
            Ok(ToolOutput {
                content: vec![JsValue::Object(object([
                    ("type", string("json")),
                    ("value", value.clone()),
                ]))],
                data: Some(value),
                metadata: Some(object([
                    ("operation", string(operation)),
                    ("revision", revision),
                ])),
                ..Default::default()
            })
        })
    }
}
