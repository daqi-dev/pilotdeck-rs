//! Session plan and checklist tools, backed by host-owned durable plan/todo.
use super::{object, schema, string};
use crate::{
    host::{HostError, modules::capability::ToolDefinition},
    native_process::elicitation::Elicitation,
    runtime::tools::{ExecutableTool, ToolError, ToolFuture, ToolOutput, ToolRunContext},
};
use pilotdeck_protocol::{JsObject, JsString, JsValue};

pub struct Tool {
    fields: JsObject,
    kind: Kind,
}
#[derive(Clone, Copy)]
pub enum Kind {
    Enter,
    Exit,
    Todo,
}
impl Tool {
    pub fn new(kind: Kind) -> Self {
        let (name, alias, description, input, capabilities) = match kind {
            Kind::Enter => (
                "enter_plan_mode",
                "EnterPlanMode",
                "Enter read-only planning and write a plan for approval.",
                r#"{"type":"object","additionalProperties":false,"properties":{}}"#,
                vec![string("plan_workflow")],
            ),
            Kind::Exit => (
                "exit_plan_mode",
                "ExitPlanMode",
                "Present a plan file for approval before implementation.",
                r#"{"type":"object","required":["plan_file_path"],"additionalProperties":false,"properties":{"plan_file_path":{"type":"string"}}}"#,
                vec![string("plan_workflow"), string("user_interaction")],
            ),
            Kind::Todo => (
                "todo_write",
                "TodoWrite",
                "Read or update the session checklist.",
                r#"{"type":"object","additionalProperties":false,"properties":{"markdown":{"type":"string"},"todos":{"type":"array","items":{"type":"object","additionalProperties":false,"properties":{"id":{"type":"string"},"content":{"type":"string"},"status":{"type":"string","enum":["pending","in_progress","completed","cancelled"]},"priority":{"type":"string"}}}},"merge":{"type":"boolean"},"reason":{"type":"string"}}}"#,
                vec![],
            ),
        };
        Self {
            fields: object([
                ("name", string(name)),
                ("aliases", JsValue::Array(vec![string(alias)])),
                ("description", string(description)),
                ("kind", string("session")),
                ("inputSchema", JsValue::Object(schema(input))),
                ("requiredRuntimeCapabilities", JsValue::Array(capabilities)),
            ]),
            kind,
        }
    }
}
impl ToolDefinition for Tool {
    fn fields(&self) -> &JsObject {
        &self.fields
    }
    fn is_trusted_mode_transition(&self) -> bool {
        matches!(self.kind, Kind::Enter | Kind::Exit)
    }
    fn is_read_only(&self, _: &JsValue) -> Result<JsValue, HostError> {
        Ok(JsValue::Bool(true))
    }
    fn is_concurrency_safe(&self, _: &JsValue) -> Result<JsValue, HostError> {
        Ok(JsValue::Bool(true))
    }
    fn requires_user_interaction(&self, _: &JsValue) -> Result<JsValue, HostError> {
        Ok(JsValue::Bool(matches!(self.kind, Kind::Exit)))
    }
    fn required_runtime_capabilities(&self) -> Option<Vec<JsString>> {
        Some(match self.kind {
            Kind::Enter => vec!["plan_workflow".into()],
            Kind::Exit => vec!["plan_workflow".into(), "user_interaction".into()],
            Kind::Todo => vec![],
        })
    }
}
impl ExecutableTool for Tool {
    fn execute(&self, input: JsValue, ctx: ToolRunContext) -> ToolFuture<ToolOutput> {
        let kind = self.kind;
        Box::pin(async move {
            match kind {
                Kind::Enter => enter(&ctx),
                Kind::Exit => exit(input, ctx).await,
                Kind::Todo => todo(input, ctx).await,
            }
        })
    }
}
fn fail(code: &str, message: impl Into<JsString>) -> ToolError {
    ToolError::new(code, message)
}
fn output(content: JsString, data: JsObject) -> ToolOutput {
    ToolOutput {
        content: vec![JsValue::Object(object([
            ("type", string("text")),
            ("text", JsValue::String(content)),
        ]))],
        data: Some(JsValue::Object(data)),
        ..Default::default()
    }
}
fn mode(ctx: &ToolRunContext) -> Option<&JsString> {
    ctx.fields
        .get("permissionMode")
        .and_then(JsValue::as_string)
}
fn enter(ctx: &ToolRunContext) -> Result<ToolOutput, ToolError> {
    if mode(ctx).is_some_and(|v| v.equals_str("plan")) {
        return Err(fail(
            "tool_execution_failed",
            "Plan mode is already active. Continue working on the plan.",
        ));
    }
    let path = ctx
        .host
        .plan_directory
        .as_ref()
        .map(|p| String::from_utf16_lossy(p.path.units()))
        .unwrap_or_else(|| ".pilotdeck/plans".into());
    Ok(output(format!("Plan mode activated. Explore the project without changing implementation files. Write a markdown plan under {path}, then call exit_plan_mode to request review.").into(),object([("requestedMode",string("plan"))])))
}
fn payload(ctx: &ToolRunContext, method: &str) -> JsObject {
    object([
        (
            "sessionId",
            ctx.fields
                .get("sessionId")
                .cloned()
                .unwrap_or(JsValue::Undefined),
        ),
        (
            "turnId",
            ctx.fields
                .get("turnId")
                .cloned()
                .unwrap_or(JsValue::Undefined),
        ),
        ("method", string(method)),
    ])
}
fn service(
    ctx: &ToolRunContext,
) -> Result<std::sync::Arc<dyn crate::host::modules::capability::PlanTodoHandler>, ToolError> {
    ctx.host
        .plan_todo
        .as_ref()
        .and_then(|v| v.downcast_ref::<crate::host::modules::capability::PlanTodoContext>())
        .map(|v| v.0.clone())
        .ok_or_else(|| fail("unsupported_tool", "The host has no plan/todo service."))
}
async fn call(
    service: &std::sync::Arc<dyn crate::host::modules::capability::PlanTodoHandler>,
    payload: JsObject,
) -> Result<JsValue, ToolError> {
    let response = service
        .call(JsValue::Object(object([(
            "payload",
            JsValue::Object(payload),
        )])))
        .await
        .map_err(|e| fail("tool_execution_failed", e.message))?;
    Ok(response
        .get("snapshot")
        .cloned()
        .unwrap_or(JsValue::Undefined))
}
async fn exit(input: JsValue, ctx: ToolRunContext) -> Result<ToolOutput, ToolError> {
    if !mode(&ctx).is_some_and(|v| v.equals_str("plan")) {
        return Err(fail(
            "tool_execution_failed",
            "exit_plan_mode can only be used while plan mode is active.",
        ));
    }
    let channel = ctx
        .host
        .elicitation
        .as_ref()
        .and_then(|v| v.downcast_ref::<Elicitation>())
        .ok_or_else(|| {
            fail(
                "unsupported_tool",
                "exit_plan_mode requires a connected user interaction channel.",
            )
        })?;
    let directory=ctx.host.plan_directory.as_ref().ok_or_else(||fail("invalid_tool_input","plan_file_path must point to a markdown file under the current project's .pilotdeck/plans directory."))?;
    let path = input
        .get("plan_file_path")
        .and_then(JsValue::as_string)
        .cloned()
        .unwrap_or_default();
    let resolved = directory
        .resolve(path.clone())
        .await
        .map_err(|e| fail("tool_execution_failed", e.message))?;
    let Some(resolved) = resolved.as_string().cloned() else {
        return Err(fail(
            "invalid_tool_input",
            "plan_file_path must point to a markdown file under the current project's .pilotdeck/plans directory.",
        ));
    };
    let plan = directory
        .read(path)
        .await
        .map_err(|e| fail("tool_execution_failed", e.message))?;
    let Some(plan) = plan.as_string().cloned() else {
        return Err(fail(
            "invalid_tool_input",
            format!(
                "Plan file does not exist or could not be read: {}",
                String::from_utf16_lossy(resolved.units())
            ),
        ));
    };
    let question = JsValue::Object(object([
        ("question", string("What should happen next?")),
        ("header", string("Plan")),
        (
            "options",
            JsValue::Array(vec![
                JsValue::Object(object([
                    ("label", string("continue_planning")),
                    ("description", string("Keep planning and revise the plan.")),
                ])),
                JsValue::Object(object([
                    ("label", string("execute_plan")),
                    (
                        "description",
                        string("Approve the plan and begin implementation."),
                    ),
                ])),
            ]),
        ),
    ]));
    let metadata = JsValue::Object(object([
        ("source", string("exit_plan_mode")),
        ("plan", JsValue::String(plan.clone())),
        ("planFilePath", JsValue::String(resolved.clone())),
    ]));
    let turn = ctx
        .fields
        .get("currentToolCallId")
        .and_then(JsValue::as_string)
        .cloned()
        .unwrap_or_default();
    let answer = channel
        .ask_custom(
            turn,
            "exit_plan_mode",
            Some("markdown"),
            JsValue::Array(vec![question]),
            Some(metadata),
            &ctx,
        )
        .await?;
    let mut action = None;
    if answer
        .get("type")
        .and_then(JsValue::as_string)
        .is_some_and(|v| v.equals_str("answered"))
        && let Some(values) = answer.get("answers").and_then(JsValue::as_object)
    {
        for (_, value) in values.own_entries() {
            let candidates = match value {
                JsValue::Array(values) => values.iter().collect::<Vec<_>>(),
                value => vec![value],
            };
            for candidate in candidates {
                if let Some(s) = candidate
                    .as_string()
                    .filter(|s| s.equals_str("continue_planning") || s.equals_str("execute_plan"))
                {
                    action = Some(s.clone());
                    break;
                }
            }
            if action.is_some() {
                break;
            }
        }
    }
    let Some(action) = action else {
        return Ok(output(
            "Exit plan mode was cancelled. Continue refining the plan.".into(),
            object([
                ("plan", JsValue::String(plan)),
                ("action", string("cancelled")),
            ]),
        ));
    };
    if action.equals_str("execute_plan") {
        let state = service(&ctx)?;
        let mut request = payload(&ctx, "mark_plan_approved");
        request.insert("plan".into(), JsValue::String(plan.clone()));
        call(&state, request).await?;
        let mut title = None;
        let mut summary = Vec::new();
        let mut found = 0;
        for line in plan.units().split(|unit| *unit == 10) {
            if title.is_none() && line.len() > 2 && line[0] == 35 && spaces(line[1]) {
                let mut index = 1;
                while index < line.len() && spaces(line[index]) {
                    index += 1;
                }
                if index < line.len() {
                    title = Some(JsString::from_utf16(line[index..].to_vec()));
                }
            }
            if found < 2
                && !crate::runtime::memory::js_trim_units(line).is_empty()
                && line.first() != Some(&35)
            {
                if found > 0 {
                    summary.push(10);
                }
                summary.extend_from_slice(line);
                found += 1;
            }
        }
        summary.truncate(200);
        let mut data = object([
            ("plan", JsValue::String(plan.clone())),
            ("action", JsValue::String(action)),
            ("requestedMode", string("default")),
            ("planFilePath", JsValue::String(resolved)),
        ]);
        if let Some(title) = title {
            data.insert("planTitle".into(), JsValue::String(title));
        }
        if !summary.is_empty() {
            data.insert(
                "planSummary".into(),
                JsValue::String(JsString::from_utf16(summary)),
            );
        }
        let mut units: Vec<u16> =
            "The plan was approved. Initialize the todo checklist before making changes.\n\n"
                .encode_utf16()
                .collect();
        units.extend_from_slice(plan.units());
        return Ok(output(JsString::from_utf16(units), data));
    }
    let feedback = answer
        .get("annotations")
        .and_then(JsValue::as_object)
        .and_then(|annotations| {
            annotations
                .own_entries()
                .into_iter()
                .find_map(|(_, annotation)| {
                    annotation
                        .get("notes")
                        .and_then(JsValue::as_string)
                        .map(trim)
                        .filter(|notes| !notes.is_empty())
                })
        });
    let mut data = object([
        ("plan", JsValue::String(plan)),
        ("action", JsValue::String(action)),
    ]);
    let mut message: Vec<u16> = "Continue planning and revise the plan before implementation."
        .encode_utf16()
        .collect();
    if let Some(notes) = feedback {
        message.extend("\n\nUser feedback:\n".encode_utf16());
        message.extend_from_slice(notes.units());
        data.insert("feedback".into(), JsValue::String(notes));
    }
    Ok(output(JsString::from_utf16(message), data))
}
fn trim(value: &JsString) -> JsString {
    JsString::from_utf16(crate::runtime::memory::js_trim_units(value.units()).to_vec())
}
fn spaces(unit: u16) -> bool {
    crate::runtime::memory::js_trim_units(std::slice::from_ref(&unit)).is_empty()
}
fn parse_markdown(value: &JsString) -> Vec<JsValue> {
    let mut result = Vec::new();
    let mut assigned = false;
    for line in value.units().split(|u| *u == 10) {
        let line = crate::runtime::memory::js_trim_units(line);
        if line.len() < 5 || !matches!(line[0], 45 | 42) || !spaces(line[1]) {
            continue;
        }
        let mut at = 1;
        while at < line.len() && spaces(line[at]) {
            at += 1;
        }
        if line.len() < at + 4
            || line[at] != 91
            || !matches!(line[at + 1], 32 | 88 | 120)
            || line[at + 2] != 93
            || !spaces(line[at + 3])
        {
            continue;
        }
        let checked = line[at + 1] != 32;
        let content = crate::runtime::memory::js_trim_units(&line[at + 4..]);
        if content.is_empty() {
            continue;
        }
        let status = if checked {
            "completed"
        } else if !assigned {
            assigned = true;
            "in_progress"
        } else {
            "pending"
        };
        result.push(JsValue::Object(object([
            (
                "id",
                JsValue::String(format!("todo-{}", result.len() + 1).into()),
            ),
            (
                "content",
                JsValue::String(JsString::from_utf16(content.to_vec())),
            ),
            ("status", string(status)),
        ])));
    }
    result
}
async fn todo(input: JsValue, ctx: ToolRunContext) -> Result<ToolOutput, ToolError> {
    let state = service(&ctx)?;
    let mut request = payload(&ctx, "read");
    let mut current = call(&state, request.clone()).await?;
    let merging = input.get("merge") == Some(&JsValue::Bool(true));
    let reason = input
        .get("reason")
        .and_then(JsValue::as_string)
        .map(trim)
        .filter(|s| !s.is_empty());
    let mode = if let Some(JsValue::Array(todos)) = input.get("todos") {
        request = payload(&ctx, "write_todos");
        request.insert("todos".into(), JsValue::Array(todos.clone()));
        request.insert("merge".into(), JsValue::Bool(merging));
        if let Some(reason) = &reason {
            request.insert("reason".into(), JsValue::String(reason.clone()));
        }
        current = call(&state, request).await?;
        "structured"
    } else if let Some(markdown) = input.get("markdown").and_then(JsValue::as_string) {
        request = payload(&ctx, "record_todo_write");
        request.insert("markdown".into(), JsValue::String(markdown.clone()));
        request.insert("todos".into(), JsValue::Array(parse_markdown(markdown)));
        if let Some(reason) = &reason {
            request.insert("reason".into(), JsValue::String(reason.clone()));
        }
        current = call(&state, request).await?;
        "markdown"
    } else {
        "read"
    };
    let todos = current
        .get("todos")
        .cloned()
        .unwrap_or(JsValue::Array(vec![]));
    let count = if let JsValue::Array(items) = &todos {
        items.len()
    } else {
        0
    };
    let diagnostics = current.get("todoDiagnostics").cloned();
    let mut text: Vec<u16> = if mode == "read" {
        "Todo list read:"
    } else {
        "Todo list updated:"
    }
    .encode_utf16()
    .collect();
    text.extend(format!("\nmode={mode} merge={merging} count={count}").encode_utf16());
    if let Some(reason) = &reason {
        text.extend("\nreason: ".encode_utf16());
        text.extend_from_slice(reason.units());
    }
    if let JsValue::Array(items) = &todos {
        if items.is_empty() {
            text.extend("\nNo todos are currently recorded.".encode_utf16());
        }
        for item in items {
            for (prefix, key) in [("\n- [", "status"), ("] id=", "id")] {
                text.extend(prefix.encode_utf16());
                if let Some(value) = item.get(key).and_then(JsValue::as_string) {
                    text.extend_from_slice(value.units());
                }
            }
            if let Some(value) = item.get("priority").and_then(JsValue::as_string) {
                text.extend(" priority=".encode_utf16());
                text.extend_from_slice(value.units());
            }
            text.push(32);
            if let Some(value) = item.get("content").and_then(JsValue::as_string) {
                text.extend_from_slice(value.units());
            }
        }
    }
    if let Some(value) = &diagnostics {
        text.extend("\n\nDiagnostics:\n".encode_utf16());
        let raw = pilotdeck_protocol::stringify_json(value)
            .map_err(|e| fail("tool_execution_failed", e.to_string()))?
            .unwrap_or_default();
        text.extend(raw.encode_utf16());
    }
    let mut data = JsObject::new();
    if let Some(v) = input.get("markdown").filter(|v| v.as_string().is_some()) {
        data.insert("markdown".into(), v.clone());
    }
    data.insert("todos".into(), todos);
    data.insert("mode".into(), string(mode));
    data.insert("merge".into(), JsValue::Bool(merging));
    if let Some(reason) = reason {
        data.insert("reason".into(), JsValue::String(reason));
    }
    if let Some(value) = diagnostics.clone() {
        data.insert("diagnostics".into(), value);
    }
    let mut result = output(JsString::from_utf16(text), data);
    let mut metadata = object([
        ("todoCount", JsValue::Number(count as f64)),
        ("mode", string(mode)),
    ]);
    if let Some(value) = diagnostics {
        metadata.insert("diagnostics".into(), value);
    }
    result.metadata = Some(metadata);
    Ok(result)
}
