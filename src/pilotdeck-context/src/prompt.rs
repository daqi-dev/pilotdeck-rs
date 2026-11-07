//! Original base and runtime prompt assembly without gateway authority.
use pilotdeck_protocol::JsValue;
#[derive(Debug, Clone)]
pub struct PromptError {
    pub message: String,
}
impl PromptError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}
pub fn build_default_system_prompt(
    permission_mode: &str,
    run_mode: Option<&str>,
    additional_working_directories: &[String],
    tools: &[JsValue],
    mcp_block: &str,
) -> Result<Vec<String>, PromptError> {
    let has_web_search = has_tool(tools, "web_search")?;
    let has_web_fetch = has_tool(tools, "web_fetch")?;
    let documentation_lookup_policy = if has_web_search {
        "When implementing code against an API, SDK, framework, CLI, config schema, or file format whose usage is not clear from local source, installed types, examples, or project docs, search for official documentation before writing or changing code. Prefer versioned official docs and existing in-repo call sites. Use web_search for discovery and web_fetch for the relevant docs page. Do not guess unfamiliar signatures, options, or output shapes when a quick lookup can resolve them. If network tools are unavailable or denied, state the uncertainty and proceed conservatively."
    } else if has_web_fetch {
        "When implementing code against an API, SDK, framework, CLI, config schema, or file format whose usage is not clear, prefer installed types, local source, examples, and project docs. If a relevant official documentation URL is already known, use web_fetch to inspect it. Otherwise state the uncertainty and proceed conservatively."
    } else {
        "When implementing code against an API, SDK, framework, CLI, config schema, or file format whose usage is not clear, prefer installed types, local source, examples, and project docs. State any remaining uncertainty and proceed conservatively."
    };
    let mut lines: Vec<String> = vec![
            "You are PilotDeck, an AI agent runtime. You execute tasks across CLI, TUI, web, and chat channels by calling structured tools and reasoning over their results.".into(),
            "Operate decisively: prefer using available tools to gather facts before answering, prefer concise replies, and surface uncertainty when present.".into(),
            String::new(),
            "Documentation lookup policy:".into(),
            documentation_lookup_policy.into(),
            String::new(),
            "Parallel delegation policy:".into(),
            "When the agent tool is available and the task contains two or more independent, repetitive, or separable workstreams, prefer launching multiple subagents in the same assistant message instead of waiting for one to finish before starting the next. Good parallel candidates include inspecting multiple files/modules, researching independent APIs, checking several artifacts, or comparing alternatives. Do not parallelize tasks that write to the same files, depend on another subagent's output, require shared ordering, or need user approval between steps. Give each subagent a self-contained prompt, distinct scope, and expected output. After all sibling results return, synthesize them yourself and decide the next step.".into(),
            String::new(),
            "Reusable script workflow:".into(),
            "When code is more than a tiny one-off command, or you expect to rerun it with changed parameters, write it to a workspace file first with write_file, then run it with bash. Prefer scripts with CLI arguments, environment variables, or a small config section so parameters can be adjusted with edit_file or command args. Do not pack large Python/JS/shell programs into bash heredocs or long `python -c` / `node -e` strings. After each run, inspect output and edit the saved script instead of regenerating a new inline command.".into(),
        ];
    let permission_line = format_permission_mode(permission_mode);
    if !permission_line.is_empty() {
        lines.push(String::new());
        lines.push(permission_line);
    }
    if let Some(run_mode_line) = format_run_mode(run_mode) {
        lines.push(run_mode_line);
    }
    if !additional_working_directories.is_empty() {
        lines.push(String::new());
        lines.push("Additional working directories you may operate in:".into());
        for dir in additional_working_directories {
            lines.push(format!("- {dir}"));
        }
    }
    if !mcp_block.is_empty() {
        lines.push(String::new());
        lines.push("Connected MCP server instructions:".into());
        lines.push(mcp_block.to_owned());
    }
    Ok(vec![lines.join("\n")])
}
fn has_tool(tools: &[pilotdeck_protocol::JsValue], name: &str) -> Result<bool, PromptError> {
    for tool in tools {
        // `tools.some((tool) => tool.name === name)`: the `name` read fails on
        // a nullish element, any other non-object reads as `undefined`, and a
        // hit short-circuits the scan before later elements are read.
        let tool_name = match tool {
            JsValue::Undefined | JsValue::Null => {
                return Err(PromptError::new(format!(
                    "Cannot read properties of {} (reading 'name')",
                    if tool.is_undefined() {
                        "undefined"
                    } else {
                        "null"
                    }
                ))
                .into());
            }
            _ => tool.get("name").cloned().unwrap_or(JsValue::Undefined),
        };
        if tool_name
            .as_string()
            .is_some_and(|value| value.equals_str(name))
        {
            return Ok(true);
        }
    }
    Ok(false)
}
pub fn build_user_context(
    cwd: &str,
    provider: &str,
    model: &str,
    permission_mode: &str,
    run_mode: Option<&str>,
    platform: &str,
    node_version: &str,
) -> Vec<String> {
    let mut lines = vec![
        "<user-context>".to_owned(),
        format!("cwd: {cwd}"),
        "IMPORTANT: When the user does not specify an explicit file path, all file paths in tool calls MUST be relative to the cwd above — use \"foo.html\", not an absolute path like \"/home/user/foo.html\". If the user explicitly provides a path, respect their choice.".to_owned(),
        format!("model: {provider}/{model}"),
        format!("permission_mode: {permission_mode}"),
    ];
    if let Some(run_mode) = run_mode.filter(|mode| !mode.is_empty()) {
        lines.push(format!("run_mode: {run_mode}"));
    }
    lines.push(format!("platform: {platform}"));
    lines.push(format!("node: {node_version}"));
    lines.push("</user-context>".into());
    vec![lines.join("\n")]
}
fn format_permission_mode(mode: &str) -> String {
    match mode {
        "default" => {
            "Permission mode: default — write/shell tools require explicit approval.".into()
        }
        "plan" => "Permission mode: plan — read-only planning mode; implementation changes are blocked at tool runtime.".into(),
        "bypassPermissions" => {
            "Permission mode: bypassPermissions — all tools are auto-approved; act conservatively."
                .into()
        }
        other => format!("Permission mode: {other}"),
    }
}
fn format_run_mode(mode: Option<&str>) -> Option<String> {
    match mode {
        Some("ask") => Some("Run mode: ask — read-only analysis mode; write/action tools are blocked at tool runtime even when permission mode is bypassPermissions.".into()),
        Some("plan") => Some("Run mode: plan — planning mode is active.".into()),
        _ => None,
    }
}
