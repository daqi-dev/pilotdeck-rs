//! StaffDeck SOP control tool. The tool is dynamically exposed and executed by
//! the SOP control port; ordinary registry execution is deliberately rejected.
use super::{object, schema, string, text};
use crate::{
    host::{HostError, HostFuture, modules::capability::ToolDefinition},
    runtime::tools::{ExecutableTool, ToolError, ToolFuture, ToolOutput, ToolRunContext},
};
use pilotdeck_protocol::{JsObject, JsString, JsValue};
use std::sync::Arc;

pub const NAME: &str = "submit_step_result";

pub struct SubmitStepResultTool {
    fields: JsObject,
}
impl SubmitStepResultTool {
    pub fn new() -> Self {
        Self {
            fields: object([
                ("name", string(NAME)),
                ("title", string("Submit SOP step result")),
                (
                    "description",
                    string(
                        "Submit the current StaffDeck SOP step result after all required PilotDeck tools have returned successfully.",
                    ),
                ),
                ("kind", string("structured_output")),
                (
                    "inputSchema",
                    JsValue::Object(schema(
                        r#"{"type":"object","required":["status","replyFragment","slotUpdates"],"additionalProperties":false,"properties":{"status":{"type":"string","enum":["completed","awaiting_user","handoff","failed","blocked","waiting_external_task"]},"replyFragment":{"type":"string"},"slotUpdates":{"type":"object","additionalProperties":true},"taskSummary":{"type":"string"},"structuredResult":{},"nextStepId":{"type":"string"}}}"#,
                    )),
                ),
            ]),
        }
    }
}
impl Default for SubmitStepResultTool {
    fn default() -> Self {
        Self::new()
    }
}
impl ToolDefinition for SubmitStepResultTool {
    fn fields(&self) -> &JsObject {
        &self.fields
    }
    fn is_read_only(&self, _: &JsValue) -> Result<JsValue, HostError> {
        Ok(JsValue::Bool(true))
    }
    fn is_concurrency_safe(&self, _: &JsValue) -> Result<JsValue, HostError> {
        Ok(JsValue::Bool(false))
    }
}
impl ExecutableTool for SubmitStepResultTool {
    fn execute(&self, _: JsValue, _: ToolRunContext) -> ToolFuture<ToolOutput> {
        Box::pin(async {
            Err(ToolError::new(
                "unsupported_tool",
                "submit_step_result is executed by the StaffDeck SOP control port.",
            ))
        })
    }
}

#[derive(Clone, Debug)]
pub struct SopProposal {
    pub status: JsString,
    pub reply_fragment: JsString,
    pub slot_updates: JsObject,
    pub task_summary: Option<JsString>,
    pub structured_result: Option<JsValue>,
    pub next_step_id: Option<JsString>,
}
impl SopProposal {
    pub fn parse(input: &JsValue) -> Result<Self, ToolError> {
        let status = input
            .get("status")
            .and_then(JsValue::as_string)
            .filter(|v| {
                [
                    "completed",
                    "awaiting_user",
                    "handoff",
                    "failed",
                    "blocked",
                    "waiting_external_task",
                ]
                .iter()
                .any(|s| v.equals_str(s))
            })
            .cloned()
            .ok_or_else(invalid)?;
        let reply_fragment = input
            .get("replyFragment")
            .and_then(JsValue::as_string)
            .filter(|v| !text(v).trim().is_empty())
            .cloned()
            .ok_or_else(invalid)?;
        let slot_updates = input
            .get("slotUpdates")
            .and_then(JsValue::as_object)
            .cloned()
            .ok_or_else(invalid)?;
        let task_summary = optional_string(input, "taskSummary")?;
        let next_step_id = optional_string(input, "nextStepId")?;
        Ok(Self {
            status,
            reply_fragment,
            slot_updates,
            task_summary,
            structured_result: input
                .get("structuredResult")
                .filter(|v| !matches!(v, JsValue::Undefined))
                .cloned(),
            next_step_id,
        })
    }
    pub fn as_value(&self) -> JsValue {
        let mut value = object([
            ("status", JsValue::String(self.status.clone())),
            (
                "replyFragment",
                JsValue::String(self.reply_fragment.clone()),
            ),
            ("slotUpdates", JsValue::Object(self.slot_updates.clone())),
        ]);
        if let Some(v) = &self.task_summary {
            value.insert("taskSummary".into(), JsValue::String(v.clone()));
        }
        if let Some(v) = &self.structured_result {
            value.insert("structuredResult".into(), v.clone());
        }
        if let Some(v) = &self.next_step_id {
            value.insert("nextStepId".into(), JsValue::String(v.clone()));
        }
        JsValue::Object(value)
    }
}
fn optional_string(input: &JsValue, key: &str) -> Result<Option<JsString>, ToolError> {
    match input.get(key) {
        None | Some(JsValue::Undefined) | Some(JsValue::Null) => Ok(None),
        Some(JsValue::String(v)) => Ok(Some(v.clone())),
        _ => Err(invalid()),
    }
}
fn invalid() -> ToolError {
    ToolError::new(
        "invalid_tool_input",
        "submit_step_result requires a valid status, non-empty replyFragment, and correctly typed known fields.",
    )
}

#[derive(Clone)]
pub struct SopOwnerResult {
    pub data: JsValue,
    pub submitted_step_id: Option<JsString>,
    pub active_step_id: Option<JsString>,
    pub sop_status: Option<JsString>,
    pub next_step_id: Option<JsString>,
    pub finish_turn: bool,
}
pub trait SopControlPort: Send + Sync {
    fn current_step_selected(&self, session_id: &JsString) -> bool;
    fn submit(
        &self,
        session_id: JsString,
        turn_id: JsString,
        proposal: SopProposal,
    ) -> HostFuture<SopOwnerResult>;
}

/// Adapter entry for the batch owner. It refuses a control call mixed with
/// ordinary tools, matching the TS executeAll ordering contract.
pub fn execute_controlled(
    port: Arc<dyn SopControlPort>,
    input: JsValue,
    context: ToolRunContext,
    ordinary_call_count: usize,
) -> ToolFuture<ToolOutput> {
    Box::pin(async move {
        if ordinary_call_count > 0 {
            return Err(ToolError::new(
                "tool_execution_failed",
                "Call submit_step_result only after the preceding tool results are available to you.",
            ));
        }
        let session = context
            .fields
            .get("sessionId")
            .and_then(JsValue::as_string)
            .cloned()
            .ok_or_else(|| ToolError::failed("SOP control requires sessionId."))?;
        if !port.current_step_selected(&session) {
            return Err(ToolError::failed(
                "No StaffDeck SOP is selected for this session.",
            ));
        }
        let turn = context
            .fields
            .get("turnId")
            .and_then(JsValue::as_string)
            .cloned()
            .ok_or_else(|| ToolError::failed("SOP control requires turnId."))?;
        let proposal = SopProposal::parse(&input)?;
        if proposal.status.equals_str("awaiting_user") && proposal.next_step_id.is_some() {
            return Err(ToolError::new(
                "invalid_tool_input",
                "AWAITING_USER_CANNOT_ADVANCE: remove nextStepId and retain the current step.",
            ));
        }
        let result = port
            .submit(session, turn, proposal)
            .await
            .map_err(|e| ToolError::failed(e.message.clone()))?;
        let mut receipt = object([
            (
                "submittedStepId",
                result
                    .submitted_step_id
                    .clone()
                    .map(JsValue::String)
                    .unwrap_or(JsValue::Null),
            ),
            (
                "activeStepId",
                result
                    .active_step_id
                    .clone()
                    .map(JsValue::String)
                    .unwrap_or(JsValue::Null),
            ),
            (
                "sopStatus",
                result
                    .sop_status
                    .clone()
                    .map(JsValue::String)
                    .unwrap_or(JsValue::Null),
            ),
            (
                "nextStepId",
                result
                    .next_step_id
                    .clone()
                    .map(JsValue::String)
                    .unwrap_or(JsValue::Null),
            ),
        ]);
        let result_status = result
            .data
            .get("status")
            .cloned()
            .unwrap_or(JsValue::Undefined);
        receipt.insert("status".into(), result_status.clone());
        receipt.insert("resultStatus".into(), result_status);
        if result
            .sop_status
            .as_ref()
            .is_some_and(|v| v.equals_str("active"))
        {
            receipt.insert("nextStepRequiresOwnSubmission".into(), JsValue::Bool(true));
        }
        Ok(ToolOutput {
            content: vec![JsValue::Object(object([
                ("type", string("json")),
                ("value", JsValue::Object(receipt)),
            ]))],
            data: Some(result.data),
            metadata: Some(object([(
                "structuredOutput",
                JsValue::Bool(result.finish_turn),
            )])),
            ..Default::default()
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn proposal_validation() {
        let input = pilotdeck_protocol::parse_json(
            r#"{"status":"completed","replyFragment":"done","slotUpdates":{}}"#,
        )
        .unwrap();
        assert!(SopProposal::parse(&input).is_ok());
        let invalid = pilotdeck_protocol::parse_json(
            r#"{"status":"completed","replyFragment":" ","slotUpdates":{}}"#,
        )
        .unwrap();
        assert_eq!(
            SopProposal::parse(&invalid).unwrap_err().code,
            "invalid_tool_input"
        );
    }
}
