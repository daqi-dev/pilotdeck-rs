//! Model-visible tool error guidance (TS errorRecovery.ts and ToolRuntime.ts).
//! Text operations preserve JavaScript UTF-16, including truncated surrogates.

use crate::host::{HostError, ModuleError};
use crate::js_value::{StackOverflow, number_string, to_string_units};
use pilotdeck_protocol::{JsObject, JsString, JsValue};
use regex::bytes::Regex;
use std::sync::LazyLock;

pub fn build_tool_error_recovery(
    code: &str,
    tool_name: &JsString,
    message: &JsString,
    cwd: &JsString,
    permission_mode: &JsString,
    details: Option<&JsObject>,
) -> JsObject {
    let issues = validation_issues(details);
    let evidence = salient_evidence(message, details, &issues);
    let summary = summarize(code, tool_name, message, &evidence, &issues);
    let class = failure_class(code, tool_name, message, details);
    let actions = unique(next_actions(
        code,
        tool_name,
        message,
        cwd,
        permission_mode,
        details,
        &issues,
    ));
    let actions = &actions[..actions.len().min(3)];
    let evidence = &evidence[..evidence.len().min(2)];
    let avoid = avoid_retry(code);
    let original = original_error(message);
    let mut advice = JsObject::new();
    advice.insert("summary".into(), JsValue::String(summary.clone()));
    advice.insert("failureClass".into(), class.into());
    advice.insert("nextActions".into(), string_array(actions));
    advice.insert("salientEvidence".into(), string_array(evidence));
    if let Some(avoid) = avoid {
        advice.insert("avoidRetryReason".into(), avoid.into());
    }
    if let Some(original) = &original {
        advice.insert("originalError".into(), JsValue::String(original.clone()));
    }
    let mut lines = vec![
        interpolate(
            "TOOL_ERROR[{}][{}][{}]",
            &[&code.into(), tool_name, &class.into()],
        ),
        interpolate("Summary: {}", &[&summary]),
    ];
    if !evidence.is_empty() {
        lines.push("Evidence:".into());
        lines.extend(evidence.iter().map(|value| interpolate("- {}", &[value])));
    }
    if let Some(avoid) = avoid {
        lines.push(format!("Do not retry unchanged: {avoid}").into());
    }
    if let Some(original) = original {
        lines.push("Original error:".into());
        lines.push(original);
    }
    if !actions.is_empty() {
        lines.push("Next actions:".into());
        lines.extend(actions.iter().enumerate().map(|(index, value)| {
            interpolate("{}. {}", &[&(index + 1).to_string().into(), value])
        }));
    }
    let mut result = JsObject::new();
    result.insert("message".into(), JsValue::String(join(&lines, "\n")));
    result.insert("advice".into(), JsValue::Object(advice));
    result
}

/// Preserve raw shell output after the bounded recovery guidance. `Err` is
/// V8's RangeError from `${value}` on a too-deep array (shared bounded
/// String(), `crate::js_value`); TS throws it out of createToolErrorResult.
pub fn format_tool_error_content(
    recovery_message: &JsString,
    details: Option<&JsObject>,
) -> Result<JsString, HostError> {
    let Some(details) = details else {
        return Ok(recovery_message.clone());
    };
    let command = string(Some(details), "command").filter(|value| !value.is_empty());
    let scalar = ["exitCode", "timedOut", "durationMs"].map(|key| {
        details
            .get_property(key)
            .filter(|value| !value.is_undefined())
    });
    let mut lines = Vec::new();
    if command.is_some() || scalar.iter().any(Option::is_some) {
        lines.push("Raw tool details:".into());
        if let Some(command) = command {
            lines.push(interpolate("- command: {}", &[command]));
        }
        for (name, value) in ["exit_code", "timed_out", "duration_ms"]
            .into_iter()
            .zip(scalar)
        {
            if let Some(value) = value {
                let value =
                    to_string_units(value).map_err(|_| ModuleError::new(StackOverflow::MESSAGE))?;
                lines.push(interpolate(
                    "- {}: {}",
                    &[&name.into(), &JsString::from_utf16(value)],
                ));
            }
        }
    }
    for (key, heading) in [
        ("diagnostic", "Diagnostic:"),
        ("stdout", "stdout:"),
        ("stderr", "stderr:"),
    ] {
        if let Some(value) = string(Some(details), key).filter(|value| !value.is_empty()) {
            lines.extend(["".into(), heading.into(), trim_end(value)]);
        }
    }
    Ok(if lines.is_empty() {
        recovery_message.clone()
    } else {
        interpolate("{}\n\n{}", &[recovery_message, &join(&lines, "\n")])
    })
}

fn summarize(
    code: &str,
    tool: &JsString,
    raw: &JsString,
    evidence: &[JsString],
    issues: &[Issue],
) -> JsString {
    if code == "invalid_tool_input" {
        if let Some(issue) = issues.first().filter(|issue| !issue.message.is_empty()) {
            return sentence(&issue.message);
        }
        let first = first_line(raw);
        if !first.is_empty() {
            return sentence(&first);
        }
        return evidence
            .first()
            .map(sentence)
            .unwrap_or_else(|| interpolate("The {} input is invalid.", &[tool]));
    }
    let template = match code {
        "tool_not_found" => Some("The model emitted a tool name that is not registered: {}."),
        "tool_unavailable" => Some("The {} capability is known but unavailable in this session."),
        "plan_mode_violation" => {
            Some("The {} call is not allowed while the agent is in plan mode.")
        }
        "ask_mode_violation" => Some("The {} call is not allowed while the agent is in ask mode."),
        _ => None,
    };
    if let Some(template) = template {
        return interpolate(template, &[tool]);
    }
    if let Some(first) = evidence.first() {
        return sentence(first);
    }
    let first = first_line(raw);
    sentence(&if first.is_empty() {
        interpolate("{} failed with {}.", &[tool, &code.into()])
    } else {
        first
    })
}

fn failure_class(
    code: &str,
    tool: &JsString,
    raw: &JsString,
    details: Option<&JsObject>,
) -> &'static str {
    if tool.equals_str("web_fetch") {
        if code == "invalid_tool_input" {
            return "fix_input";
        }
        if equals(details, "stage", "http_fetch") {
            let status = number(details, "status");
            if matches!(status, Some(401.0 | 403.0)) {
                return "ask_user";
            }
            if matches!(status, Some(404.0 | 410.0)) {
                return "fix_input";
            }
            if retry_status(status) || pattern(2, raw) {
                return "retry_later";
            }
            return "environment_issue";
        }
        if equals(details, "stage", "secondary_model") {
            return "retry_later";
        }
    }
    match code {
        "invalid_tool_input" | "file_not_found" | "file_conflict" => return "fix_input",
        "result_too_large" => return "reduce_scope",
        "tool_unavailable" => return "tool_unavailable",
        "tool_not_found" | "unsupported_tool" | "plan_mode_violation" | "ask_mode_violation" => {
            return "switch_tool";
        }
        "permission_denied"
        | "permission_required"
        | "permission_cancelled"
        | "path_not_allowed"
        | "setup_required" => return "ask_user",
        "tool_timeout" | "tool_aborted" => return "retry_later",
        _ => {}
    }
    if tool.equals_str("bash") || code == "tool_execution_failed" {
        let mut parts = vec![raw.clone()];
        parts.extend(
            ["stderr", "stdout", "message"]
                .iter()
                .filter_map(|key| string(details, key))
                .cloned(),
        );
        let haystack = join(&parts, "\n");
        if pattern(0, &haystack) {
            return "ask_user";
        }
        if pattern(1, &haystack) {
            return "fix_input";
        }
        if pattern(2, &haystack) {
            return "retry_later";
        }
    }
    // EADDRINUSE, missing commands, refused connections, and full disks share
    // the same classification as the generic fallback.
    "environment_issue"
}

#[allow(clippy::too_many_arguments)]
fn next_actions(
    code: &str,
    tool: &JsString,
    raw: &JsString,
    cwd: &JsString,
    mode: &JsString,
    details: Option<&JsObject>,
    issues: &[Issue],
) -> Vec<JsString> {
    if tool.equals_str("web_fetch") && code != "invalid_tool_input" {
        if equals(details, "stage", "http_fetch") {
            let status = number(details, "status");
            return if matches!(status, Some(401.0 | 403.0)) {
                strings(&[
                    "The site denied access or requires authorization; ask the user for credentials/permission or use a public source.",
                    "Do not summarize the error page as if it were the requested content.",
                ])
            } else if matches!(status, Some(404.0 | 410.0)) {
                strings(&[
                    "Verify the URL or search for an updated URL before retrying.",
                    "Use another source if the original page has moved or was removed.",
                ])
            } else if retry_status(status) {
                strings(&[
                    "Retry later, reduce request frequency, or use another source.",
                    "Do not treat the HTTP error page as page content.",
                ])
            } else {
                strings(&[
                    "Inspect the HTTP status and try a different source or access path before retrying.",
                    "Do not treat the HTTP error page as page content.",
                ])
            };
        }
        if equals(details, "stage", "secondary_model") {
            return strings(&[
                "Retry with mode \"raw\" to inspect the fetched markdown without secondary model processing.",
                "Try the LLM fetch again later if the secondary model/provider was unavailable.",
            ]);
        }
    }
    let values: &[&str] = match code {
        "invalid_tool_input" => return input_actions(tool, raw, issues),
        "tool_not_found" => &["Use a registered canonical tool name from the current tool list."],
        "tool_unavailable" => &[
            "Configure the missing capability or use another available tool.",
            "Do not retry this tool until its configuration or runtime becomes available.",
        ],
        "plan_mode_violation" => &[
            "Do not retry this write/action tool while in plan mode.",
            "Use read-only tools or respond with a plan; request a mode change only if the user wants execution.",
        ],
        "ask_mode_violation" => &[
            "Do not retry this write/action tool while in ask mode.",
            "Use read-only tools or ask the user to change mode if they want execution.",
        ],
        "permission_required" => {
            &["Pause tool execution and ask the user for approval with a concise reason."]
        }
        "permission_denied" | "permission_cancelled" => &[
            "Do not retry the same action. Choose a lower-privilege alternative or ask the user how to proceed.",
        ],
        "path_not_allowed" => {
            return vec![
                interpolate("Use a path inside the workspace root: {}.", &[cwd]),
                "If the outside path is essential, explain why and ask the user for access.".into(),
            ];
        }
        "file_not_found" => {
            &["Verify the path with glob/grep/read-only inspection before retrying."]
        }
        "result_too_large" => &["Reduce the requested scope and fetch a smaller result."],
        "tool_timeout" => {
            &["Break the operation into smaller steps or retry with a narrower scope."]
        }
        "setup_required" => {
            &["Tell the user what configuration is missing and wait for it to be provided."]
        }
        "unsupported_tool" => {
            &["Switch to another available tool or explain that this capability is not configured."]
        }
        _ => {
            return vec![interpolate(
                "Inspect the evidence, change the approach, then retry only with corrected inputs. Current permission mode: {}.",
                &[mode],
            )];
        }
    };
    strings(values)
}

fn input_actions(tool: &JsString, raw: &JsString, issues: &[Issue]) -> Vec<JsString> {
    let mut parts = vec![raw.clone()];
    parts.extend(
        issues
            .iter()
            .map(|issue| interpolate("{}: {}", &[&issue.path, &issue.message])),
    );
    let haystack = join(&parts, "\n");
    if pattern(3, &haystack) {
        return vec![
            "Call read_file with the same file_path to establish the current file snapshot before writing.".into(),
            interpolate("Retry {} only after reading the file, or use edit_file for a focused change based on the current contents.", &[tool]),
        ];
    }
    if pattern(4, &haystack) {
        return vec![
            "Call read_file with the same file_path again to refresh the file snapshot.".into(),
            interpolate(
                "Retry {} using the refreshed contents so you do not overwrite concurrent changes.",
                &[tool],
            ),
        ];
    }
    if pattern(5, &haystack) {
        return strings(&[
            "Call read_file with the same file_path and copy the exact current text you need to replace.",
            "Retry edit_file with a precise old_string that appears exactly once in the current file.",
        ]);
    }
    if pattern(6, &haystack) {
        return strings(&[
            "Use a more specific old_string that uniquely identifies the intended edit, including nearby context if needed.",
            "If every occurrence should change, set replace_all to true and ensure that broad replacement is intended.",
        ]);
    }
    if tool.equals_str("bash") && pattern(7, &haystack) {
        return strings(&[
            "Use timeout=600000 or less for foreground bash.",
            "For a finite long-running command that should finish, use task_create and then task_wait to block for completion.",
            "For long-lived services or watchers, use task_create with task_output for progress and task_stop for cleanup.",
        ]);
    }
    if tool.equals_str("bash") && pattern(8, &haystack) {
        return strings(&[
            "Run short commands directly in foreground bash with a timeout of 600000ms or less.",
            "For finite background work, use task_create and then task_wait so completion output returns to the model context.",
            "For long-lived services or watchers, use task_create with task_output for progress checks and task_stop for cleanup.",
        ]);
    }
    if let Some(issue) = issues.iter().find(|issue| issue.code == "required") {
        let mut result = vec![interpolate(
            "Include the required parameter `{}` in the next {} call.",
            &[&clean_path(&issue.path), tool],
        )];
        if pattern(9, &haystack) {
            result.push("Create a smaller but complete draft first, then continue with focused follow-up edits or patches.".into());
        }
        return result;
    }
    for (code, template) in [
        (
            "invalid_type",
            "Change `{}` to the expected type before retrying: {}.",
        ),
        (
            "unknown_property",
            "Remove or rename the unexpected parameter `{}`; use only parameters from the tool schema.",
        ),
        (
            "invalid_enum",
            "Use one of the allowed values for `{}`: {}.",
        ),
    ] {
        if let Some(issue) = issues.iter().find(|issue| issue.code == code) {
            let path = clean_path(&issue.path);
            return vec![interpolate(template, &[&path, &sentence(&issue.message)])];
        }
    }
    if let Some(issue) = issues.first() {
        return vec![interpolate(
            "Fix `{}`: {}",
            &[&clean_path(&issue.path), &sentence(&issue.message)],
        )];
    }
    strings(&[
        "Fix the specific invalid argument described in the error message before calling the tool again.",
    ])
}

fn avoid_retry(code: &str) -> Option<&'static str> {
    Some(match code {
        "permission_required" => {
            "This tool requires user approval; repeated calls cannot grant approval."
        }
        "permission_denied" => "The action was denied by policy or a hook.",
        "permission_cancelled" => "The user cancelled this action.",
        "path_not_allowed" => "The path policy will continue blocking this location.",
        "setup_required" => "The missing setup must be completed outside this tool call.",
        "tool_unavailable" => "This known tool is unavailable; repeated calls cannot enable it.",
        "plan_mode_violation" => {
            "Plan mode blocks this class of tool until execution mode is restored."
        }
        "ask_mode_violation" => {
            "Ask mode blocks this class of tool until execution mode is restored."
        }
        _ => return None,
    })
}

struct Issue {
    path: JsString,
    code: &'static str,
    message: JsString,
}

fn validation_issues(details: Option<&JsObject>) -> Vec<Issue> {
    let Some(JsValue::Array(issues)) = details.and_then(|details| details.get_property("issues"))
    else {
        return Vec::new();
    };
    issues
        .iter()
        .filter_map(|value| {
            // Arrays pass JavaScript's typeof object guard, with default fields.
            if !matches!(value, JsValue::Object(_) | JsValue::Array(_)) {
                return None;
            }
            let object = value.as_object();
            let code = string(object, "code");
            Some(Issue {
                path: string(object, "path")
                    .cloned()
                    .unwrap_or_else(|| "input".into()),
                code: [
                    "required",
                    "unknown_property",
                    "invalid_type",
                    "invalid_enum",
                    "invalid_schema",
                ]
                .into_iter()
                .find(|known| code.is_some_and(|code| code.equals_str(known)))
                .unwrap_or("invalid_schema"),
                message: string(object, "message")
                    .cloned()
                    .unwrap_or_else(|| "Invalid tool input.".into()),
            })
        })
        .collect()
}

fn salient_evidence(raw: &JsString, details: Option<&JsObject>, issues: &[Issue]) -> Vec<JsString> {
    let mut evidence: Vec<_> = issues
        .iter()
        .map(|issue| interpolate("{}: {}", &[&clean_path(&issue.path), &issue.message]))
        .collect();
    evidence.extend(
        lines(raw)
            .into_iter()
            .map(|line| trim(&line))
            .filter(|line| !line.is_empty() && !error_header(line))
            .take(2),
    );
    if let Some(status) = number(details, "status") {
        let status: JsString = number_string(status).into();
        let status_text = string(details, "statusText").filter(|value| !value.is_empty());
        evidence.push(if let Some(text) = status_text {
            interpolate("HTTP status: {} {}", &[&status, text])
        } else {
            interpolate("HTTP status: {}", &[&status])
        });
    }
    if let Some(retry) = number(details, "retryAfterMs") {
        evidence.push(interpolate(
            "Retry-After: {}ms",
            &[&number_string(retry).into()],
        ));
    }
    for (key, template) in [
        ("bodyPreview", "Body preview: {}"),
        ("userHint", "Provider hint: {}"),
    ] {
        if let Some(value) = string(details, key).filter(|value| !value.is_empty()) {
            evidence.push(interpolate(template, &[value]));
        }
    }
    if let Some(stderr) = string(details, "stderr").filter(|value| !value.is_empty()) {
        evidence.push(first_line(stderr));
    }
    unique(
        evidence
            .into_iter()
            .filter(|value| !value.is_empty())
            .map(|value| sentence(&value))
            .collect(),
    )
}

fn original_error(raw: &JsString) -> Option<JsString> {
    let cleaned = trim(&join(
        &lines(raw)
            .into_iter()
            .map(|line| trim_end(&line))
            .filter(|line| !error_header(&trim(line)))
            .collect::<Vec<_>>(),
        "\n",
    ));
    if cleaned.is_empty() {
        return None;
    }
    if cleaned.len_utf16() <= 2000 {
        return Some(cleaned);
    }
    let prefix = trim_end(&JsString::from_utf16(cleaned.units()[..2000].to_vec()));
    Some(interpolate(
        "{}\n... [original error truncated; {} chars total]",
        &[&prefix, &cleaned.len_utf16().to_string().into()],
    ))
}

fn clean_path(path: &JsString) -> JsString {
    let units = path.units();
    let start = if units.first() == Some(&36) {
        1 + usize::from(units.get(1) == Some(&46))
    } else {
        0
    };
    if start == units.len() {
        "input".into()
    } else {
        JsString::from_utf16(units[start..].to_vec())
    }
}

fn sentence(value: &JsString) -> JsString {
    let mut compact = Vec::new();
    let mut pending_space = false;
    for &unit in value.units() {
        if white(unit) {
            pending_space = !compact.is_empty();
        } else {
            if pending_space {
                compact.push(32);
            }
            compact.push(unit);
            pending_space = false;
        }
    }
    if compact.len() > 220 {
        compact.truncate(217);
        while compact.last().is_some_and(|unit| white(*unit)) {
            compact.pop();
        }
        compact.extend([46, 46, 46]);
    }
    JsString::from_utf16(compact)
}

fn white(unit: u16) -> bool {
    matches!(unit, 9..=13 | 32 | 160 | 0x1680 | 0x2000..=0x200a | 0x2028 | 0x2029 | 0x202f | 0x205f | 0x3000 | 0xfeff)
}
fn trim(value: &JsString) -> JsString {
    let units = value.units();
    let start = units
        .iter()
        .position(|unit| !white(*unit))
        .unwrap_or(units.len());
    let end = units
        .iter()
        .rposition(|unit| !white(*unit))
        .map_or(start, |index| index + 1);
    JsString::from_utf16(units[start..end].to_vec())
}
fn trim_end(value: &JsString) -> JsString {
    let units = value.units();
    let end = units
        .iter()
        .rposition(|unit| !white(*unit))
        .map_or(0, |index| index + 1);
    JsString::from_utf16(units[..end].to_vec())
}
fn lines(value: &JsString) -> Vec<JsString> {
    // All callers trim at least the end, so removing CR at the final segment
    // is equivalent to the source's /\r?\n/ split followed by that trim.
    value
        .units()
        .split(|unit| *unit == 10)
        .map(|line| {
            let line = line.strip_suffix(&[13]).unwrap_or(line);
            JsString::from_utf16(line.to_vec())
        })
        .collect()
}
fn first_line(value: &JsString) -> JsString {
    lines(value)
        .into_iter()
        .map(|line| trim(&line))
        .find(|line| !line.is_empty())
        .unwrap_or_default()
}
fn error_header(value: &JsString) -> bool {
    let prefix = "tool_error[";
    value
        .units()
        .iter()
        .take(prefix.len())
        .map(|&unit| {
            if (65..=90).contains(&unit) {
                unit + 32
            } else {
                unit
            }
        })
        .eq(prefix.encode_utf16())
}
fn unique(values: Vec<JsString>) -> Vec<JsString> {
    let mut output = Vec::new();
    for value in values {
        if !output.contains(&value) {
            output.push(value);
        }
    }
    output
}
fn strings(values: &[&str]) -> Vec<JsString> {
    values.iter().map(|value| (*value).into()).collect()
}
fn string_array(values: &[JsString]) -> JsValue {
    JsValue::Array(values.iter().cloned().map(JsValue::String).collect())
}
fn string<'a>(details: Option<&'a JsObject>, key: &str) -> Option<&'a JsString> {
    details?.get_property(key)?.as_string()
}
fn number(details: Option<&JsObject>, key: &str) -> Option<f64> {
    details?.get_property(key)?.as_number()
}
fn equals(details: Option<&JsObject>, key: &str, value: &str) -> bool {
    string(details, key).is_some_and(|text| text.equals_str(value))
}
fn retry_status(status: Option<f64>) -> bool {
    status.is_some_and(|status| status == 408.0 || status == 429.0 || status >= 500.0)
}
fn join(values: &[JsString], separator: &str) -> JsString {
    let mut result = Vec::new();
    for (index, value) in values.iter().enumerate() {
        if index > 0 {
            result.extend(separator.encode_utf16());
        }
        result.extend_from_slice(value.units());
    }
    JsString::from_utf16(result)
}
fn interpolate(template: &str, values: &[&JsString]) -> JsString {
    let mut output = Vec::new();
    let mut parts = template.split("{}");
    output.extend(parts.next().unwrap_or_default().encode_utf16());
    for (part, value) in parts.zip(values) {
        output.extend_from_slice(value.units());
        output.extend(part.encode_utf16());
    }
    JsString::from_utf16(output)
}

fn pattern(index: usize, value: &JsString) -> bool {
    static PATTERNS: LazyLock<Vec<Regex>> = LazyLock::new(|| {
        [
        "Permission denied|EACCES|EPERM",
        "No such file or directory|ENOENT|NameError|ReferenceError|SyntaxError|TypeError|ModuleNotFoundError|Cannot find module",
        "timed? ?out|ETIMEDOUT|ESOCKETTIMEDOUT",
        "File has not been read yet",
        "File has changed since the last read",
        "String to replace not found|old_string.*not found|not appear in the target file",
        "Found [0-9]+ matches of old_string|multiple matches|replace_all",
        "timeout [0-9]+ms exceeds|timeout .*exceeds|exceeds the maximum|maximum of 600000",
        "background|nohup|disown|setsid|task_create|task_wait",
        "content|output truncated|token limit|output token",
    ].iter().map(|pattern| Regex::new(&format!("(?i-u:{pattern})")).expect("static ASCII error recovery regex")).collect()
    });
    // Only predicate matches use a decoded view. Every output retains the
    // original UTF-16; JS dot excludes all four ECMAScript line terminators.
    let text =
        String::from_utf16_lossy(value.units()).replace(['\r', '\u{2028}', '\u{2029}'], "\n");
    PATTERNS[index].is_match(text.as_bytes())
}
