//! The public agent tool delegates to the turn-owned native fork port.
use super::{field, object, schema, string};
use crate::{
    host::{HostError, modules::capability::ToolDefinition},
    runtime::tools::{ExecutableTool, ToolError, ToolFuture, ToolOutput, ToolRunContext},
};
use pilotdeck_protocol::{JsObject, JsString, JsValue};
use std::sync::Arc;

pub struct ForkRequest {
    pub definition_id: JsString,
    pub directive: JsString,
    pub subagent_id: JsString,
    pub tool_call_id: Option<JsString>,
    pub timeout_ms: f64,
    pub context: ToolRunContext,
}

pub struct ForkReport {
    pub markdown: JsString,
    pub usage: JsValue,
    pub turns: f64,
    pub duration_ms: f64,
    pub parsed: Option<JsObject>,
    pub subagent_session_id: Option<JsString>,
    pub transcript_relative_path: Option<JsString>,
}

pub trait SubagentFork: Send + Sync {
    fn definitions(&self) -> Vec<JsString>;
    fn is_allowed_definition(&self, id: &JsString) -> bool {
        self.definitions().contains(id)
    }
    fn depth(&self) -> f64;
    fn max_depth(&self) -> f64;
    fn is_background(&self, _definition: &JsString) -> bool {
        false
    }
    fn has_background_launcher(&self) -> bool {
        false
    }
    fn fork(&self, request: ForkRequest) -> ToolFuture<ForkReport>;
    fn launch_background(&self, _request: ForkRequest) -> ToolFuture<JsString> {
        Box::pin(async {
            Err(ToolError::failed(
                "Background subagent launcher is unavailable.",
            ))
        })
    }
}

pub struct SubagentForkApi(pub Arc<dyn SubagentFork>);

pub struct AgentTool {
    fields: JsObject,
}

pub fn agent_tool() -> Arc<dyn ExecutableTool> {
    Arc::new(AgentTool::new())
}

impl Default for AgentTool {
    fn default() -> Self {
        Self::new()
    }
}

impl AgentTool {
    pub fn new() -> Self {
        Self::for_depth(1.0, 0.0)
    }

    pub fn for_depth(max: f64, depth: f64) -> Self {
        let nested = depth.max(0.0) + 1.0 < max;
        let access = if nested {
            "all parent tools; nested delegation is available within the configured depth cap"
        } else {
            "all parent tools except nested agent launch"
        };
        let summary = if nested {
            "General-purpose subagent for complex research/synthesis tasks. Has broad parent-tool access; nested delegation follows the configured depth cap."
        } else {
            "General-purpose subagent for complex research/synthesis tasks. Has broad parent-tool access except nested subagent launch."
        };
        let description = [
            "Launch a new subagent to handle a focused multi-step task.",
            "",
            "Use this tool when a bounded piece of work would benefit from an autonomous helper instead of keeping every intermediate step in the parent agent's context.",
            "",
            "Provide:",
            "- `description`: a short 3-5 word label for the task.",
            "- `prompt`: the full directive for the subagent. Write it like a complete briefing: include the goal, relevant context, constraints, and what good output looks like.",
            "- `subagent_type` (optional): choose a built-in preset. If omitted, `general-purpose` is used.",
            "",
            "Available built-in subagent types:",
            &format!("- general-purpose: {summary} Tools: {access}.\n- explore: Read-only exploration subagent. Inspects files, runs grep/glob, and may run safe shell commands. Cannot edit files. Tools: read_file, grep, glob, bash.\n- plan: Read-only planning subagent. Inspects code via read/grep/glob and produces a step-by-step plan. Tools: read_file, grep, glob."),
            "",
            "The subagent returns one structured report with these sections: `Scope`, `Result`, `Key files`, `Files changed`, and `Issues`.",
            "",
            "Runtime behavior:",
            "- Multiple independent agent calls in one assistant message may run concurrently; batch sibling investigations when their scopes do not depend on each other.",
            "- Inside the AgentLoop, this runs a real forked subagent with its own scoped tool loop.",
            "- In stand-alone runtimes and some tests, it falls back to a single model call that preserves the same high-level subagent intent.",
        ].join("\n");
        let mut fields = object([
            ("name", string("agent")),
            (
                "aliases",
                JsValue::Array(vec![string("Agent"), string("Task")]),
            ),
            ("description", string(description)),
            ("kind", string("agent")),
            ("maxResultBytes", JsValue::Number(200_000.0)),
            ("isOpenWorld", JsValue::Bool(true)),
            (
                "inputSchema",
                JsValue::Object(schema(
                    r#"{"type":"object","required":["description","prompt"],"additionalProperties":false,"properties":{"description":{"type":"string","description":"Short 3-5 word task summary used to label the subagent run."},"prompt":{"type":"string","description":"Detailed directive for the subagent. Include the goal, relevant context, constraints, and desired output; do not assume the subagent already knows why the task matters."},"subagent_type":{"type":"string","description":"Optional subagent preset. Public built-ins: 'general-purpose' (full tool access), 'explore' (read-only investigation with read_file/grep/glob/bash), or 'plan' (read-only planning with read_file/grep/glob). Some runtimes may also expose additional presets such as 'verify'. Defaults to 'general-purpose' when omitted. Legacy 'general_purpose' is still accepted for compatibility."},"subagentType":{"type":"string","description":"Deprecated legacy alias for subagent_type. Prefer subagent_type."}}}"#,
                )),
            ),
        ]);
        fields.set_native_tag("PilotDeckBuiltinAgent");
        Self { fields }
    }
}

pub(crate) fn for_child_scope(
    parent: Arc<dyn ExecutableTool>,
    max: f64,
    depth: f64,
) -> Option<Arc<dyn ExecutableTool>> {
    if parent.fields().native_tag() != Some("PilotDeckBuiltinAgent") {
        return None;
    }
    let mut fields = parent.fields().clone();
    fields.insert(
        "description".into(),
        AgentTool::for_depth(max, depth)
            .fields
            .get("description")
            .unwrap()
            .clone(),
    );
    Some(Arc::new(DescribedAgent { parent, fields }))
}

struct DescribedAgent {
    parent: Arc<dyn ExecutableTool>,
    fields: JsObject,
}
impl ToolDefinition for DescribedAgent {
    fn fields(&self) -> &JsObject {
        &self.fields
    }
    fn is_read_only(&self, input: &JsValue) -> Result<JsValue, HostError> {
        self.parent.is_read_only(input)
    }
    fn is_concurrency_safe(&self, input: &JsValue) -> Result<JsValue, HostError> {
        self.parent.is_concurrency_safe(input)
    }
    fn requires_user_interaction(&self, input: &JsValue) -> Result<JsValue, HostError> {
        self.parent.requires_user_interaction(input)
    }
    fn required_runtime_capabilities(&self) -> Option<Vec<JsString>> {
        self.parent.required_runtime_capabilities()
    }
}
impl ExecutableTool for DescribedAgent {
    fn validate_input(
        &self,
        input: JsValue,
        context: ToolRunContext,
    ) -> ToolFuture<Option<Vec<JsObject>>> {
        self.parent.validate_input(input, context)
    }
    fn execute(&self, input: JsValue, context: ToolRunContext) -> ToolFuture<ToolOutput> {
        self.parent.execute(input, context)
    }
}

impl ToolDefinition for AgentTool {
    fn fields(&self) -> &JsObject {
        &self.fields
    }
    fn is_read_only(&self, _: &JsValue) -> Result<JsValue, HostError> {
        Ok(JsValue::Bool(false))
    }
    fn is_concurrency_safe(&self, _: &JsValue) -> Result<JsValue, HostError> {
        Ok(JsValue::Bool(true))
    }
    fn required_runtime_capabilities(&self) -> Option<Vec<JsString>> {
        Some(vec!["subagent_fork".into()])
    }
}

fn failure(message: impl Into<JsString>, code: &str) -> ToolError {
    let mut error = ToolError::failed(message);
    error.details = Some(object([("errorCode", string(code))]));
    error
}

fn aborted() -> ToolError {
    ToolError::new("tool_aborted", "agent subagent aborted before completion.")
}

pub fn normalize_requested_type(value: &JsString) -> Option<JsString> {
    let trimmed = crate::runtime::tools::builtin::text(value)
        .trim_matches(crate::metadata::js_space)
        .to_owned();
    if trimmed.is_empty() {
        return None;
    }
    Some(match trimmed.to_lowercase().as_str() {
        "general-purpose" | "general_purpose" | "general purpose" => "general-purpose".into(),
        "explore" | "explorer" => "explore".into(),
        "plan" => "plan".into(),
        "verify" => "verify".into(),
        _ => trimmed.into(),
    })
}

impl ExecutableTool for AgentTool {
    fn execute(&self, input: JsValue, context: ToolRunContext) -> ToolFuture<ToolOutput> {
        Box::pin(async move {
            let explicit = match field(&input, "subagent_type") {
                JsValue::Undefined | JsValue::Null => field(&input, "subagentType"),
                value => value,
            };
            let mut requested = explicit
                .as_string()
                .and_then(normalize_requested_type)
                .unwrap_or_else(|| "general-purpose".into());
            if requested.equals_str("general-purpose")
                && (context.fields.get("runMode") == Some(&JsValue::from("ask"))
                    || context
                        .fields
                        .get("permissionContext")
                        .and_then(|v| v.get("mode"))
                        == Some(&JsValue::from("plan")))
            {
                requested = "explore".into();
            }
            let Some(port) = context
                .host
                .subagent
                .as_ref()
                .and_then(|p| p.downcast_ref::<SubagentForkApi>())
                .map(|api| api.0.clone())
            else {
                return fallback(input, context, requested).await;
            };
            let definitions = port.definitions();
            if !port.is_allowed_definition(&requested) {
                return Err(ToolError::new(
                    "invalid_tool_input",
                    format!(
                        "Unknown subagent_type \"{}\". Available: {}.",
                        super::text(&requested),
                        definitions
                            .iter()
                            .map(super::text)
                            .collect::<Vec<_>>()
                            .join(", ")
                    ),
                ));
            }
            let depth = context
                .fields
                .get("subagentDepth")
                .and_then(JsValue::as_number)
                .unwrap_or_else(|| port.depth());
            if depth >= port.max_depth() {
                return Err(failure(
                    format!(
                        "subagent_depth_exceeded (depth={}, max={}); nested fork rejected.",
                        crate::js_value::number_string(depth),
                        crate::js_value::number_string(port.max_depth())
                    ),
                    "subagent_depth_exceeded",
                ));
            }
            let subagent_id: JsString = uuid::Uuid::new_v4().to_string().into();
            let request = ForkRequest {
                definition_id: requested.clone(),
                directive: field(&input, "prompt")
                    .as_string()
                    .cloned()
                    .unwrap_or_default(),
                subagent_id: subagent_id.clone(),
                tool_call_id: context
                    .fields
                    .get("currentToolCallId")
                    .and_then(JsValue::as_string)
                    .cloned(),
                timeout_ms: context
                    .fields
                    .get("subagentTimeoutMs")
                    .and_then(JsValue::as_number)
                    .unwrap_or(3_600_000.0),
                context: context.clone(),
            };
            let description = field(&input, "description").clone();
            if port.is_background(&requested) {
                if !port.has_background_launcher() {
                    return Err(ToolError::new(
                        "unsupported_tool",
                        format!(
                            "Background subagent {} requires a Gateway background-subagent launcher.",
                            super::text(&requested)
                        ),
                    ));
                }
                if context.is_aborted() {
                    return Err(ToolError::new(
                        "tool_aborted",
                        "agent subagent aborted before launch.",
                    ));
                }
                let id = port.launch_background(request).await.map_err(|e| {
                    failure(
                        format!(
                            "background agent subagent failed to launch: {}",
                            super::text(&e.message)
                        ),
                        "subagent_launch_failed",
                    )
                })?;
                let data = object([
                    ("subagentType", string(requested.clone())),
                    ("description", description.clone()),
                    (
                        "text",
                        string(format!(
                            "Background subagent started as task {}.",
                            super::text(&id)
                        )),
                    ),
                    ("backgroundTaskId", string(id.clone())),
                ]);
                return Ok(output(
                    data,
                    object([
                        ("subagent", string(requested)),
                        ("subagentId", string(subagent_id)),
                        ("forkMode", string("background")),
                        ("backgroundTaskId", string(id)),
                    ]),
                ));
            }
            let report = port.fork(request).await.map_err(|e| {
                if context.is_aborted() {
                    aborted()
                } else {
                    failure(
                        format!("agent subagent failed: {}", super::text(&e.message)),
                        "subagent_execution_failed",
                    )
                }
            })?;
            if context.is_aborted() {
                return Err(aborted());
            }
            let mut data = object([
                ("subagentType", string(requested.clone())),
                ("description", description),
                ("text", string(report.markdown)),
                ("usage", report.usage),
                ("turns", JsValue::Number(report.turns)),
                ("durationMs", JsValue::Number(report.duration_ms)),
                (
                    "parsed",
                    report
                        .parsed
                        .map(JsValue::Object)
                        .unwrap_or(JsValue::Undefined),
                ),
            ]);
            let mut metadata = object([
                ("subagent", string(requested)),
                ("subagentId", string(subagent_id)),
                ("forkMode", string("full")),
                ("turns", JsValue::Number(report.turns)),
                ("durationMs", JsValue::Number(report.duration_ms)),
            ]);
            for (key, value) in [
                ("subagentSessionId", report.subagent_session_id),
                ("transcriptRelativePath", report.transcript_relative_path),
            ] {
                if let Some(value) = value.filter(|v| !v.is_empty()) {
                    data.insert(key.into(), string(value.clone()));
                    metadata.insert(key.into(), string(value));
                }
            }
            Ok(output(data, metadata))
        })
    }
}

async fn fallback(
    input: JsValue,
    context: ToolRunContext,
    requested: JsString,
) -> Result<ToolOutput, ToolError> {
    use super::web::fetch_tool::{SecondaryModelEvent, WebFetchModelPort};
    let prompt = match super::text(&requested).as_str() {
        "general-purpose" => {
            "You are a general-purpose subagent inside PilotDeck. Read the user's instructions, reason carefully, and produce a single concise text answer. Do not ask follow-up questions; do your best with the information given."
        }
        "plan" => {
            "You are a planning subagent inside PilotDeck. Given a task, return a numbered plan of concrete steps a developer or operator could follow. Be specific. Do not perform the steps yourself; return the plan only."
        }
        "verify" => {
            "You are a verification subagent inside PilotDeck. Given a proposal, change, or claim, return a structured critique with: (1) specific concerns, (2) recommended checks, (3) overall verdict. Be rigorous; flag risks even if minor."
        }
        "explore" => {
            "You are an exploration subagent inside PilotDeck. Given a topic, return a structured overview: (a) common approaches, (b) trade-offs between them, (c) recommended next steps for someone unfamiliar with the area."
        }
        _ => {
            return Err(ToolError::new(
                "invalid_tool_input",
                format!(
                    "Unknown subagent_type \"{}\". Available: general-purpose, plan, verify, explore.",
                    super::text(&requested)
                ),
            ));
        }
    };
    let model = context.host.model.as_ref().and_then(|port| port.downcast_ref::<WebFetchModelPort>()).ok_or_else(|| ToolError::new("unsupported_tool", "agent tool requires a model client. Configure dependencies.model on AgentRuntimeDependencies, pass createAgentTool({ model }), or wire context.subagent for full-fork mode."))?;
    let directive = field(&input, "prompt").clone();
    let description = field(&input, "description").clone();
    let request = JsValue::Object(object([
        ("provider", string("pilotdeck")),
        ("model", string("moonshotai/kimi-k2.6")),
        ("systemPrompt", string(prompt)),
        (
            "messages",
            JsValue::Array(vec![JsValue::Object(object([
                ("role", string("user")),
                (
                    "content",
                    JsValue::Array(vec![JsValue::Object(object([
                        ("type", string("text")),
                        ("text", directive.clone()),
                    ]))]),
                ),
            ]))]),
        ),
        ("maxOutputTokens", JsValue::Number(65_536.0)),
        ("stream", JsValue::Bool(true)),
        (
            "metadata",
            JsValue::Object(object([
                ("subagent", string(requested.clone())),
                ("description", description.clone()),
            ])),
        ),
    ]));
    let mut stream = model.0.stream(request, Some(context.clone()));
    let mut text = Vec::new();
    let mut usage = JsValue::Undefined;
    while let Some(item) = stream.next().await {
        match item {
            SecondaryModelEvent::Event(event) => {
                if context.is_aborted() {
                    return Err(aborted());
                }
                match field(&event, "type")
                    .as_string()
                    .map(super::text)
                    .as_deref()
                {
                    Some("text_delta") => {
                        let delta = crate::js_value::to_string_units(field(&event, "text"))
                            .map_err(|_| ToolError::failed("Maximum call stack size exceeded"))?;
                        if text.len().saturating_add(delta.len())
                            > crate::runtime::model::MAX_STRING_LENGTH
                        {
                            return Err(ToolError::failed("Invalid string length"));
                        }
                        text.extend_from_slice(&delta);
                    }
                    Some("usage") => usage = field(&event, "usage").clone(),
                    Some("error") => {
                        let error = field(&event, "error");
                        let message = crate::js_value::display(field(error, "message"))
                            .map_err(|_| ToolError::failed("Maximum call stack size exceeded"))?;
                        let mut failure =
                            ToolError::failed(format!("agent subagent model error: {message}"));
                        failure.details =
                            Some(object([("errorCode", field(error, "code").clone())]));
                        return Err(failure);
                    }
                    _ => {}
                }
            }
            SecondaryModelEvent::Throw { is_error, value } => {
                let message = crate::js_value::display(if is_error {
                    field(&value, "message")
                } else {
                    &value
                })
                .map_err(|_| ToolError::failed("Maximum call stack size exceeded"))?;
                return Err(ToolError::failed(message));
            }
        }
    }
    let text = String::from_utf16_lossy(&text);
    let text = text.trim_matches(crate::metadata::js_space);
    Ok(output(
        object([
            ("subagentType", string(requested.clone())),
            ("description", description),
            (
                "text",
                string(if text.is_empty() {
                    "(empty subagent response)"
                } else {
                    text
                }),
            ),
            ("usage", usage),
        ]),
        object([
            ("subagent", string(requested)),
            ("forkMode", string("fallback")),
            ("provider", string("pilotdeck")),
            ("model", string("moonshotai/kimi-k2.6")),
            (
                "promptBytes",
                JsValue::Number(
                    directive
                        .as_string()
                        .map(|text| super::text(text).len())
                        .unwrap_or(0) as f64,
                ),
            ),
        ]),
    ))
}

fn output(data: JsObject, metadata: JsObject) -> ToolOutput {
    let label = |key| {
        data.get(key)
            .and_then(JsValue::as_string)
            .map(super::text)
            .unwrap_or_default()
    };
    ToolOutput {
        content: vec![
            JsValue::Object(object([
                ("type", string("text")),
                (
                    "text",
                    string(format!(
                        "[{}] {}\n\n{}",
                        label("subagentType"),
                        label("description"),
                        label("text")
                    )),
                ),
            ])),
            JsValue::Object(object([
                ("type", string("json")),
                ("value", JsValue::Object(data.clone())),
            ])),
        ],
        data: Some(JsValue::Object(data)),
        metadata: Some(metadata),
        supplemental_messages: None,
    }
}
