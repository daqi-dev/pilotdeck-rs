//! Enter/exit plan mode with explicit plan-file review and elicitation.
use super::todo_write::TodoSession;
use super::{object, schema, string, text};
use crate::{
    host::{HostError, modules::capability::ToolDefinition},
    runtime::{
        interaction::channel::QuestionChannel,
        tools::{ExecutableTool, ToolError, ToolFuture, ToolOutput, ToolRunContext},
    },
};
use pilotdeck_protocol::{JsObject, JsString, JsValue};

#[derive(Clone, Copy)]
enum Operation {
    Enter,
    Exit,
}
pub struct PlanModeTool {
    operation: Operation,
    fields: JsObject,
}
impl PlanModeTool {
    pub fn enter() -> Self {
        Self::new(Operation::Enter)
    }
    pub fn exit() -> Self {
        Self::new(Operation::Exit)
    }
    fn new(operation: Operation) -> Self {
        let (name, aliases, description, schema_text) = match operation {
            Operation::Enter => (
                "enter_plan_mode",
                "EnterPlanMode",
                "Enter plan mode for complex tasks requiring exploration and design.",
                r#"{"type":"object","additionalProperties":false,"properties":{}}"#,
            ),
            Operation::Exit => (
                "exit_plan_mode",
                "ExitPlanMode",
                "Signal that your plan is complete and ready for user review.",
                r#"{"type":"object","required":["plan_file_path"],"additionalProperties":false,"properties":{"plan_file_path":{"type":"string"}}}"#,
            ),
        };
        Self {
            operation,
            fields: object([
                ("name", string(name)),
                ("aliases", JsValue::Array(vec![string(aliases)])),
                ("description", string(description)),
                ("kind", string("session")),
                (
                    "shouldDefer",
                    JsValue::Bool(matches!(operation, Operation::Exit)),
                ),
                ("inputSchema", JsValue::Object(schema(schema_text))),
            ]),
        }
    }
}
impl ToolDefinition for PlanModeTool {
    fn fields(&self) -> &JsObject {
        &self.fields
    }
    fn is_read_only(&self, _: &JsValue) -> Result<JsValue, HostError> {
        Ok(JsValue::Bool(true))
    }
    fn is_concurrency_safe(&self, _: &JsValue) -> Result<JsValue, HostError> {
        Ok(JsValue::Bool(true))
    }
    fn requires_user_interaction(&self, _: &JsValue) -> Result<JsValue, HostError> {
        Ok(JsValue::Bool(matches!(self.operation, Operation::Exit)))
    }
    fn required_runtime_capabilities(&self) -> Option<Vec<JsString>> {
        Some(match self.operation {
            Operation::Enter => vec!["plan_workflow".into()],
            Operation::Exit => vec!["plan_workflow".into(), "user_interaction".into()],
        })
    }
}
impl ExecutableTool for PlanModeTool {
    fn execute(&self, input: JsValue, context: ToolRunContext) -> ToolFuture<ToolOutput> {
        let operation = self.operation;
        Box::pin(async move {
            match operation {
                Operation::Enter => enter(&context),
                Operation::Exit => exit(&input, &context).await,
            }
        })
    }
}
fn enter(context: &ToolRunContext) -> Result<ToolOutput, ToolError> {
    let directory = context.host.plan_directory.as_ref().map(|v| text(&v.path));
    if mode(context) == "plan" {
        return Err(ToolError::failed(already_active(directory.as_deref())));
    }
    Ok(ToolOutput {
        content: vec![JsValue::Object(object([
            ("type", string("text")),
            ("text", string(activated(directory.as_deref()))),
        ]))],
        data: Some(JsValue::Object(object([("requestedMode", string("plan"))]))),
        ..Default::default()
    })
}
async fn exit(input: &JsValue, context: &ToolRunContext) -> Result<ToolOutput, ToolError> {
    if mode(context) != "plan" {
        return Err(ToolError::failed(
            "exit_plan_mode can only be used while plan mode is active.",
        ));
    }
    let channel = context
        .host
        .elicitation
        .as_ref()
        .and_then(|v| v.downcast_ref::<QuestionChannel>())
        .ok_or_else(|| {
            ToolError::new(
                "unsupported_tool",
                "exit_plan_mode requires a connected user interaction channel.",
            )
        })?;
    let directory=context.host.plan_directory.as_ref().ok_or_else(||ToolError::new("invalid_tool_input","plan_file_path must point to a markdown file under the current project's .pilotdeck/plans directory."))?;
    let path=input.get("plan_file_path").and_then(JsValue::as_string).cloned().ok_or_else(||ToolError::new("invalid_tool_input","plan_file_path must point to a markdown file under the current project's .pilotdeck/plans directory."))?;
    let resolved = directory
        .resolve(path.clone())
        .await
        .map_err(|e| ToolError::failed(e.message.clone()))?;
    let resolved=resolved.as_string().cloned().ok_or_else(||ToolError::new("invalid_tool_input","plan_file_path must point to a markdown file under the current project's .pilotdeck/plans directory."))?;
    let plan = directory
        .read(path)
        .await
        .map_err(|e| ToolError::failed(e.message.clone()))?;
    let plan = plan.as_string().cloned().ok_or_else(|| {
        ToolError::new(
            "invalid_tool_input",
            format!(
                "Plan file does not exist or could not be read: {}",
                text(&resolved)
            ),
        )
    })?;
    let plan_text = text(&plan).trim().to_owned();
    if plan_text.is_empty() {
        return Err(ToolError::new(
            "invalid_tool_input",
            "Plan file is empty. Write your plan first before calling exit_plan_mode.",
        ));
    }
    let request = JsValue::Object(object([
        ("previewFormat", string("markdown")),
        (
            "questions",
            JsValue::Array(vec![JsValue::Object(object([
                ("question", string("What should happen next?")),
                ("header", string("Plan")),
                (
                    "options",
                    JsValue::Array(vec![
                        JsValue::Object(object([
                            ("label", string("continue_planning")),
                            (
                                "description",
                                string(
                                    "Keep planning and update the plan before any implementation starts.",
                                ),
                            ),
                        ])),
                        JsValue::Object(object([
                            ("label", string("execute_plan")),
                            (
                                "description",
                                string("Leave plan mode and let the agent execute this plan."),
                            ),
                        ])),
                    ]),
                ),
            ]))]),
        ),
        (
            "metadata",
            JsValue::Object(object([
                ("source", string("exit_plan_mode")),
                ("plan", string(plan_text.clone())),
                ("planFilePath", JsValue::String(resolved.clone())),
            ])),
        ),
    ]));
    let answer = channel.ask(&request, context).await?;
    if answer
        .get("type")
        .and_then(JsValue::as_string)
        .is_some_and(|v| v.equals_str("cancelled"))
    {
        return Ok(cancelled(&plan_text));
    }
    let action = answer
        .get("answers")
        .and_then(JsValue::as_object)
        .and_then(|answers| {
            answers.entries().iter().find_map(|(_, value)| match value {
                JsValue::String(v)
                    if v.equals_str("continue_planning") || v.equals_str("execute_plan") =>
                {
                    Some(text(v))
                }
                JsValue::Array(values) => values.iter().find_map(|v| {
                    v.as_string()
                        .filter(|v| {
                            v.equals_str("continue_planning") || v.equals_str("execute_plan")
                        })
                        .map(text)
                }),
                _ => None,
            })
        });
    let feedback = answer
        .get("annotations")
        .and_then(JsValue::as_object)
        .and_then(|values| {
            values.entries().iter().find_map(|(_, value)| {
                value
                    .get("notes")
                    .and_then(JsValue::as_string)
                    .map(text)
                    .map(|v| v.trim().to_owned())
                    .filter(|v| !v.is_empty())
            })
        });
    match action.as_deref() {
        Some("execute_plan") => {
            if let Some(todo) = context
                .host
                .plan_todo
                .as_ref()
                .and_then(|v| v.downcast_ref::<TodoSession>())
            {
                let turn = context
                    .fields
                    .get("turnId")
                    .and_then(JsValue::as_string)
                    .cloned()
                    .unwrap_or_else(|| "plan".into());
                todo.mark_plan_approved(&plan, &turn)?;
            }
            let title = plan_text
                .lines()
                .find_map(|line| line.strip_prefix("# "))
                .map(str::trim)
                .filter(|v| !v.is_empty());
            let summary = plan_text
                .lines()
                .filter(|line| !line.trim().is_empty() && !line.starts_with('#'))
                .take(2)
                .collect::<Vec<_>>()
                .join("\n");
            let mut data = object([
                ("plan", string(plan_text.clone())),
                ("action", string("execute_plan")),
                ("requestedMode", string("default")),
                ("planFilePath", JsValue::String(resolved.clone())),
            ]);
            if let Some(title) = title {
                data.insert("planTitle".into(), string(title));
            }
            if !summary.is_empty() {
                data.insert(
                    "planSummary".into(),
                    string(summary.chars().take(200).collect::<String>()),
                );
            }
            Ok(ToolOutput {
                content: vec![JsValue::Object(object([
                    ("type", string("text")),
                    ("text", string(approved(&plan_text, &resolved))),
                ]))],
                data: Some(JsValue::Object(data)),
                ..Default::default()
            })
        }
        Some("continue_planning") => {
            let mut data = object([
                ("plan", string(plan_text)),
                ("action", string("continue_planning")),
            ]);
            if let Some(value) = feedback.as_ref() {
                data.insert("feedback".into(), string(value.as_str()));
            }
            Ok(ToolOutput {
                content: vec![JsValue::Object(object([
                    ("type", string("text")),
                    (
                        "text",
                        string(format!(
                            "The user wants to continue planning before implementation. Stay in plan mode, refine the plan file, and call exit_plan_mode again when the updated plan is ready.{}",
                            feedback
                                .map(|v| format!("\n\nUser feedback:\n{v}"))
                                .unwrap_or_else(
                                    || "\n\nNo additional feedback was provided.".into()
                                )
                        )),
                    ),
                ]))],
                data: Some(JsValue::Object(data)),
                ..Default::default()
            })
        }
        _ => Ok(cancelled(&plan_text)),
    }
}
fn mode(context: &ToolRunContext) -> String {
    context
        .fields
        .get("permissionMode")
        .and_then(JsValue::as_string)
        .map(text)
        .unwrap_or_default()
}
fn activated(directory: Option<&str>) -> String {
    format!(
        "Plan mode activated. You are now in a read-only exploration and planning phase.\n\n{}## What To Do\n1. Explore the codebase using read_file, grep, glob to understand existing patterns and structure\n2. Identify the key files, functions, and data flows relevant to the task\n3. Design your implementation approach - consider trade-offs between alternatives\n4. Create and refine your markdown plan file\n5. When your plan is ready, call exit_plan_mode with the plan_file_path\n\n## Rules\n- DO NOT call bash with write commands\n- You MAY use ask_user_question to clarify requirements or choose between approaches\n- Focus on understanding before proposing - read first, plan second",
        directory
            .map(|v| format!(
                "## Plan Directory\nCreate your plan as a markdown file under: {v}\n\n"
            ))
            .unwrap_or_default()
    )
}
fn already_active(directory: Option<&str>) -> String {
    format!("Plan mode is already active.{}Stay in read-only exploration/planning until you are ready to call exit_plan_mode.",directory.map(|v|format!("\n\nYour plan directory is: {v}\nContinue refining markdown plan files under that directory, then submit one explicitly with exit_plan_mode(plan_file_path).\n\n")).unwrap_or_else(||"\n\n".into()))
}
fn approved(plan: &str, path: &JsString) -> String {
    format!(
        "User has approved your plan. You can now start coding.\nDo NOT output any confirmation text; proceed directly with todo_write and implementation.\nBefore using any non-read-only tool, you MUST call todo_write with a markdown checklist derived from the approved plan.\n\nSubmitted plan file: {}\n\n## Approved Plan\n{plan}",
        text(path)
    )
}
fn cancelled(plan: &str) -> ToolOutput {
    ToolOutput {
        content: vec![JsValue::Object(object([
            ("type", string("text")),
            (
                "text",
                string(
                    "Exit plan mode was cancelled. Stay in plan mode and continue refining the plan file.",
                ),
            ),
        ]))],
        data: Some(JsValue::Object(object([
            ("plan", string(plan)),
            ("action", string("cancelled")),
        ]))),
        ..Default::default()
    }
}
