//! Fresh pure port of fixed Original tool/execution/errorRecovery.ts.
//! Inputs are immutable wire/domain values, not native getter/prototype graphs.
use crate::ProjectionError;
use crate::tool_error_recovery::{ToolErrorRecoveryInput, ToolErrorRecoveryResult};
use pilotdeck_protocol::{JsObject, JsString, JsValue, model::BoundarySnapshot, stringify_json};
use std::collections::HashSet;

fn cat(parts: &[JsString]) -> JsString {
    JsString::from_utf16(
        parts
            .iter()
            .flat_map(|s| s.units().iter().copied())
            .collect(),
    )
}
fn wrap(before: &str, value: &JsString, after: &str) -> JsString {
    cat(&[before.into(), value.clone(), after.into()])
}
fn join(values: &[JsString], separator: &str) -> JsString {
    let mut units = Vec::new();
    for (i, value) in values.iter().enumerate() {
        if i > 0 {
            units.extend(separator.encode_utf16());
        }
        units.extend_from_slice(value.units());
    }
    JsString::from_utf16(units)
}
fn whitespace(u: u16) -> bool {
    matches!(u, 0x0009..=0x000d | 0x0020 | 0x00a0 | 0x1680 | 0x2000..=0x200a
        | 0x2028 | 0x2029 | 0x202f | 0x205f | 0x3000 | 0xfeff)
}
fn trim(value: &JsString, left: bool) -> JsString {
    let u = value.units();
    let start = if left {
        u.iter().position(|x| !whitespace(*x)).unwrap_or(u.len())
    } else {
        0
    };
    let end = u
        .iter()
        .rposition(|x| !whitespace(*x))
        .map_or(start, |i| (i + 1).max(start));
    JsString::from_utf16(u[start..end].to_vec())
}
fn sentence(value: &JsString) -> JsString {
    let mut out = Vec::new();
    let mut in_space = false;
    for u in value.units() {
        if whitespace(*u) {
            if !in_space {
                out.push(32);
            }
            in_space = true;
        } else {
            out.push(*u);
            in_space = false;
        }
    }
    let compact = trim(&JsString::from_utf16(out), true);
    if compact.units().len() <= 220 {
        compact
    } else {
        cat(&[
            trim(
                &JsString::from_utf16(compact.units()[..217].to_vec()),
                false,
            ),
            "...".into(),
        ])
    }
}
fn lines(value: &JsString) -> Vec<JsString> {
    value
        .units()
        .split(|u| *u == 10)
        .map(|u| {
            let u = if u.last() == Some(&13) {
                &u[..u.len() - 1]
            } else {
                u
            };
            JsString::from_utf16(u.to_vec())
        })
        .collect()
}
fn first_line(value: &JsString) -> JsString {
    lines(value)
        .into_iter()
        .map(|s| trim(&s, true))
        .find(|s| !s.is_empty())
        .unwrap_or_else(|| "".into())
}
fn fold(u: u16) -> u16 {
    if (65..=90).contains(&u) { u + 32 } else { u }
}
fn equal_i(units: &[u16], literal: &str) -> bool {
    units.len() == literal.len()
        && units
            .iter()
            .zip(literal.bytes())
            .all(|(a, b)| fold(*a) == fold(u16::from(b)))
}
fn contains_i(value: &JsString, literal: &str) -> bool {
    value
        .units()
        .windows(literal.len())
        .any(|w| equal_i(w, literal))
}
fn prefix_error(value: &JsString) -> bool {
    let prefix = "TOOL_ERROR[";
    value
        .units()
        .get(..prefix.len())
        .is_some_and(|u| equal_i(u, prefix))
}
fn any_i(value: &JsString, literals: &[&str]) -> bool {
    literals.iter().any(|p| contains_i(value, p))
}
fn line_terminator(u: u16) -> bool {
    matches!(u, 10 | 13 | 0x2028 | 0x2029)
}
fn dot_between(value: &JsString, before: &str, after: &str) -> bool {
    let u = value.units();
    for start in 0..=u.len().saturating_sub(before.len()) {
        if !u
            .get(start..start + before.len())
            .is_some_and(|w| equal_i(w, before))
        {
            continue;
        }
        let tail = &u[start + before.len()..];
        let end = tail
            .iter()
            .position(|x| line_terminator(*x))
            .unwrap_or(tail.len());
        if contains_i(&JsString::from_utf16(tail[..end].to_vec()), after) {
            return true;
        }
    }
    false
}
fn digit_match(value: &JsString, before: &str, after: &str) -> bool {
    let u = value.units();
    for start in 0..=u.len().saturating_sub(before.len()) {
        if !u
            .get(start..start + before.len())
            .is_some_and(|w| equal_i(w, before))
        {
            continue;
        }
        let from = start + before.len();
        let mut end = from;
        while u.get(end).is_some_and(|x| (48..=57).contains(x)) {
            end += 1;
        }
        if end > from
            && u.get(end..end + after.len())
                .is_some_and(|w| equal_i(w, after))
        {
            return true;
        }
    }
    false
}
fn timeout(value: &JsString) -> bool {
    any_i(
        value,
        &[
            "timeout",
            "time out",
            "timedout",
            "timed out",
            "ETIMEDOUT",
            "ESOCKETTIMEDOUT",
        ],
    )
}
fn unique(values: Vec<JsString>) -> Vec<JsString> {
    let mut seen = HashSet::new();
    values
        .into_iter()
        .filter(|s| seen.insert(s.clone()))
        .collect()
}
fn field<'a>(details: Option<&'a JsValue>, name: &str) -> Option<&'a JsValue> {
    details.and_then(|v| v.get(name))
}
fn string<'a>(details: Option<&'a JsValue>, name: &str) -> Option<&'a JsString> {
    field(details, name).and_then(JsValue::as_string)
}
fn number(details: Option<&JsValue>, name: &str) -> Option<f64> {
    field(details, name).and_then(JsValue::as_number)
}
fn number_string(n: f64) -> Result<JsString, ProjectionError> {
    if n.is_nan() {
        return Ok("NaN".into());
    }
    if n == f64::INFINITY {
        return Ok("Infinity".into());
    }
    if n == f64::NEG_INFINITY {
        return Ok("-Infinity".into());
    }
    stringify_json(&JsValue::Number(n))
        .map_err(|e| ProjectionError::Codec {
            message: e.to_string().into(),
        })?
        .map(|s| s.into())
        .ok_or_else(|| ProjectionError::Codec {
            message: "Number stringify was undefined".into(),
        })
}
struct Issue {
    path: JsString,
    message: JsString,
    code: JsString,
}
fn issues(details: Option<&JsValue>) -> Vec<Issue> {
    let Some(JsValue::Array(values)) = field(details, "issues") else {
        return Vec::new();
    };
    values
        .iter()
        .filter(|v| matches!(v, JsValue::Object(_) | JsValue::Array(_)))
        .map(|v| {
            let path = string(Some(v), "path")
                .cloned()
                .unwrap_or_else(|| "input".into());
            let message = string(Some(v), "message")
                .cloned()
                .unwrap_or_else(|| "Invalid tool input.".into());
            let code = string(Some(v), "code")
                .cloned()
                .filter(|s| {
                    [
                        "required",
                        "unknown_property",
                        "invalid_type",
                        "invalid_enum",
                        "invalid_schema",
                    ]
                    .iter()
                    .any(|c| s.equals_str(c))
                })
                .unwrap_or_else(|| "invalid_schema".into());
            Issue {
                path,
                message,
                code,
            }
        })
        .collect()
}
fn clean_path(path: &JsString) -> JsString {
    let u = path.units();
    let start = if u.first() == Some(&36) {
        if u.get(1) == Some(&46) { 2 } else { 1 }
    } else {
        0
    };
    if start == u.len() {
        "input".into()
    } else {
        JsString::from_utf16(u[start..].to_vec())
    }
}
fn issue_evidence(issue: &Issue) -> JsString {
    cat(&[clean_path(&issue.path), ": ".into(), issue.message.clone()])
}
fn haystack(message: &JsString, details: Option<&JsValue>) -> JsString {
    let mut v = vec![message.clone()];
    for key in ["stderr", "stdout", "message"] {
        if let Some(s) = string(details, key) {
            v.push(s.clone());
        }
    }
    join(
        &v.into_iter().filter(|s| !s.is_empty()).collect::<Vec<_>>(),
        "\n",
    )
}
fn evidence(input: &ToolErrorRecoveryInput) -> Result<Vec<JsString>, ProjectionError> {
    let details = input.details.as_ref().map(BoundarySnapshot::value);
    let mut v = issues(details)
        .iter()
        .map(issue_evidence)
        .collect::<Vec<_>>();
    v.extend(
        lines(&input.message)
            .into_iter()
            .map(|s| trim(&s, true))
            .filter(|s| !s.is_empty() && !prefix_error(s))
            .take(2),
    );
    if let Some(status) = number(details, "status") {
        let mut parts = vec!["HTTP status: ".into(), number_string(status)?];
        if let Some(s) = string(details, "statusText").filter(|s| !s.is_empty()) {
            parts.extend([" ".into(), s.clone()]);
        }
        v.push(cat(&parts));
    }
    if let Some(n) = number(details, "retryAfterMs") {
        v.push(cat(&[
            "Retry-After: ".into(),
            number_string(n)?,
            "ms".into(),
        ]));
    }
    for (key, label) in [
        ("bodyPreview", "Body preview: "),
        ("userHint", "Provider hint: "),
    ] {
        if let Some(s) = string(details, key).filter(|s| !s.is_empty()) {
            v.push(wrap(label, s, ""));
        }
    }
    if let Some(s) = string(details, "stderr").filter(|s| !s.is_empty()) {
        v.push(first_line(s));
    }
    Ok(unique(
        v.into_iter()
            .filter(|s| !s.is_empty())
            .map(|s| sentence(&s))
            .collect(),
    ))
}
fn summary(input: &ToolErrorRecoveryInput, evidence: &[JsString]) -> JsString {
    if input.code.equals_str("invalid_tool_input") {
        if let Some(issue) = issues(input.details.as_ref().map(BoundarySnapshot::value))
            .first()
            .filter(|i| !i.message.is_empty())
        {
            return sentence(&issue.message);
        }
        let first = first_line(&input.message);
        if !first.is_empty() {
            return sentence(&first);
        }
        if let Some(first) = evidence.first() {
            return sentence(first);
        }
        return wrap("The ", &input.tool_name, " input is invalid.");
    }
    for (code, before, after) in [
        (
            "tool_not_found",
            "The model emitted a tool name that is not registered: ",
            ".",
        ),
        (
            "tool_unavailable",
            "The ",
            " capability is known but unavailable in this session.",
        ),
        (
            "plan_mode_violation",
            "The ",
            " call is not allowed while the agent is in plan mode.",
        ),
        (
            "ask_mode_violation",
            "The ",
            " call is not allowed while the agent is in ask mode.",
        ),
    ] {
        if input.code.equals_str(code) {
            return wrap(before, &input.tool_name, after);
        }
    }
    if let Some(first) = evidence.first() {
        return sentence(first);
    }
    let first = first_line(&input.message);
    if !first.is_empty() {
        sentence(&first)
    } else {
        sentence(&cat(&[
            input.tool_name.clone(),
            " failed with ".into(),
            input.code.clone(),
            ".".into(),
        ]))
    }
}
fn classify(input: &ToolErrorRecoveryInput) -> &'static str {
    let code = &input.code;
    let details = input.details.as_ref().map(BoundarySnapshot::value);
    if input.tool_name.equals_str("web_fetch") {
        if code.equals_str("invalid_tool_input") {
            return "fix_input";
        }
        if string(details, "stage").is_some_and(|s| s.equals_str("http_fetch")) {
            let n = number(details, "status");
            if matches!(n, Some(401.0 | 403.0)) {
                return "ask_user";
            }
            if matches!(n, Some(404.0 | 410.0)) {
                return "fix_input";
            }
            if matches!(n, Some(408.0 | 429.0))
                || n.is_some_and(|v| v >= 500.0)
                || timeout(&input.message)
                || contains_i(&input.message, "network timeout")
            {
                return "retry_later";
            }
            return "environment_issue";
        }
        if string(details, "stage").is_some_and(|s| s.equals_str("secondary_model")) {
            return "retry_later";
        }
    }
    for (class, codes) in [
        (
            "fix_input",
            &["invalid_tool_input", "file_not_found", "file_conflict"][..],
        ),
        ("reduce_scope", &["result_too_large"][..]),
        ("tool_unavailable", &["tool_unavailable"][..]),
        (
            "switch_tool",
            &[
                "tool_not_found",
                "unsupported_tool",
                "plan_mode_violation",
                "ask_mode_violation",
            ][..],
        ),
        (
            "ask_user",
            &[
                "permission_denied",
                "permission_required",
                "permission_cancelled",
                "path_not_allowed",
                "setup_required",
            ][..],
        ),
        ("retry_later", &["tool_timeout", "tool_aborted"][..]),
    ] {
        if codes.iter().any(|c| code.equals_str(c)) {
            return class;
        }
    }
    if input.tool_name.equals_str("bash") || code.equals_str("tool_execution_failed") {
        let h = haystack(&input.message, details);
        if any_i(&h, &["Permission denied", "EACCES", "EPERM"]) {
            return "ask_user";
        }
        if any_i(
            &h,
            &[
                "No such file or directory",
                "ENOENT",
                "NameError",
                "ReferenceError",
                "SyntaxError",
                "TypeError",
                "ModuleNotFoundError",
                "Cannot find module",
            ],
        ) {
            return "fix_input";
        }
        if timeout(&h) {
            return "retry_later";
        }
        if any_i(&h, &["EADDRINUSE", "address already in use"])
            || dot_between(&h, "port ", "already in use")
        {
            return "environment_issue";
        }
    }
    "environment_issue"
}
fn texts(values: &[&str]) -> Vec<JsString> {
    values.iter().map(|s| (*s).into()).collect()
}
fn web_actions(input: &ToolErrorRecoveryInput) -> Vec<JsString> {
    if input.code.equals_str("invalid_tool_input") {
        return Vec::new();
    }
    let d = input.details.as_ref().map(BoundarySnapshot::value);
    if string(d, "stage").is_some_and(|s| s.equals_str("http_fetch")) {
        let n = number(d, "status");
        if matches!(n, Some(401.0 | 403.0)) {
            return texts(&[
                "The site denied access or requires authorization; ask the user for credentials/permission or use a public source.",
                "Do not summarize the error page as if it were the requested content.",
            ]);
        }
        if matches!(n, Some(404.0 | 410.0)) {
            return texts(&[
                "Verify the URL or search for an updated URL before retrying.",
                "Use another source if the original page has moved or was removed.",
            ]);
        }
        if matches!(n, Some(408.0 | 429.0)) || n.is_some_and(|v| v >= 500.0) {
            return texts(&[
                "Retry later, reduce request frequency, or use another source.",
                "Do not treat the HTTP error page as page content.",
            ]);
        }
        return texts(&[
            "Inspect the HTTP status and try a different source or access path before retrying.",
            "Do not treat the HTTP error page as page content.",
        ]);
    }
    if string(d, "stage").is_some_and(|s| s.equals_str("secondary_model")) {
        return texts(&[
            "Retry with mode \"raw\" to inspect the fetched markdown without secondary model processing.",
            "Try the LLM fetch again later if the secondary model/provider was unavailable.",
        ]);
    }
    Vec::new()
}
fn invalid_actions(input: &ToolErrorRecoveryInput) -> Vec<JsString> {
    let issues = issues(input.details.as_ref().map(BoundarySnapshot::value));
    let mut values = vec![input.message.clone()];
    values.extend(
        issues
            .iter()
            .map(|i| cat(&[i.path.clone(), ": ".into(), i.message.clone()])),
    );
    let h = join(&values, "\n");
    if contains_i(&h, "File has not been read yet") {
        return vec!["Call read_file with the same file_path to establish the current file snapshot before writing.".into(),wrap("Retry ",&input.tool_name," only after reading the file, or use edit_file for a focused change based on the current contents.")];
    }
    if contains_i(&h, "File has changed since the last read") {
        return vec![
            "Call read_file with the same file_path again to refresh the file snapshot.".into(),
            wrap(
                "Retry ",
                &input.tool_name,
                " using the refreshed contents so you do not overwrite concurrent changes.",
            ),
        ];
    }
    if any_i(
        &h,
        &[
            "String to replace not found",
            "not appear in the target file",
        ],
    ) || dot_between(&h, "old_string", "not found")
    {
        return texts(&[
            "Call read_file with the same file_path and copy the exact current text you need to replace.",
            "Retry edit_file with a precise old_string that appears exactly once in the current file.",
        ]);
    }
    if digit_match(&h, "Found ", " matches of old_string")
        || any_i(&h, &["multiple matches", "replace_all"])
    {
        return texts(&[
            "Use a more specific old_string that uniquely identifies the intended edit, including nearby context if needed.",
            "If every occurrence should change, set replace_all to true and ensure that broad replacement is intended.",
        ]);
    }
    if input.tool_name.equals_str("bash")
        && (digit_match(&h, "timeout ", "ms exceeds")
            || dot_between(&h, "timeout ", "exceeds")
            || any_i(&h, &["exceeds the maximum", "maximum of 600000"]))
    {
        return texts(&[
            "Use timeout=600000 or less for foreground bash.",
            "For a finite long-running command that should finish, use task_create and then task_wait to block for completion.",
            "For long-lived services or watchers, use task_create with task_output for progress and task_stop for cleanup.",
        ]);
    }
    if input.tool_name.equals_str("bash")
        && any_i(
            &h,
            &[
                "background",
                "nohup",
                "disown",
                "setsid",
                "task_create",
                "task_wait",
            ],
        )
    {
        return texts(&[
            "Run short commands directly in foreground bash with a timeout of 600000ms or less.",
            "For finite background work, use task_create and then task_wait so completion output returns to the model context.",
            "For long-lived services or watchers, use task_create with task_output for progress checks and task_stop for cleanup.",
        ]);
    }
    for kind in [
        "required",
        "invalid_type",
        "unknown_property",
        "invalid_enum",
    ] {
        if let Some(i) = issues.iter().find(|i| i.code.equals_str(kind)) {
            let param = wrap("`", &clean_path(&i.path), "`");
            return match kind {
                "required" => {
                    let mut a = vec![cat(&[
                        "Include the required parameter ".into(),
                        param,
                        " in the next ".into(),
                        input.tool_name.clone(),
                        " call.".into(),
                    ])];
                    if any_i(
                        &h,
                        &["content", "output truncated", "token limit", "output token"],
                    ) {
                        a.push("Create a smaller but complete draft first, then continue with focused follow-up edits or patches.".into());
                    }
                    a
                }
                "invalid_type" => vec![cat(&[
                    "Change ".into(),
                    param,
                    " to the expected type before retrying: ".into(),
                    sentence(&i.message),
                    ".".into(),
                ])],
                "unknown_property" => vec![cat(&[
                    "Remove or rename the unexpected parameter ".into(),
                    param,
                    "; use only parameters from the tool schema.".into(),
                ])],
                _ => vec![cat(&[
                    "Use one of the allowed values for ".into(),
                    param,
                    ": ".into(),
                    sentence(&i.message),
                    ".".into(),
                ])],
            };
        }
    }
    if let Some(i) = issues.first() {
        return vec![cat(&[
            "Fix ".into(),
            wrap("`", &clean_path(&i.path), "`"),
            ": ".into(),
            sentence(&i.message),
        ])];
    }
    texts(&[
        "Fix the specific invalid argument described in the error message before calling the tool again.",
    ])
}
fn actions(input: &ToolErrorRecoveryInput) -> Vec<JsString> {
    if input.tool_name.equals_str("web_fetch") {
        let a = web_actions(input);
        if !a.is_empty() {
            return a;
        }
    }
    match input.code.units() {
        _ if input.code.equals_str("invalid_tool_input") => invalid_actions(input),
        _ if input.code.equals_str("tool_not_found") => {
            texts(&["Use a registered canonical tool name from the current tool list."])
        }
        _ if input.code.equals_str("tool_unavailable") => texts(&[
            "Configure the missing capability or use another available tool.",
            "Do not retry this tool until its configuration or runtime becomes available.",
        ]),
        _ if input.code.equals_str("plan_mode_violation") => texts(&[
            "Do not retry this write/action tool while in plan mode.",
            "Use read-only tools or respond with a plan; request a mode change only if the user wants execution.",
        ]),
        _ if input.code.equals_str("ask_mode_violation") => texts(&[
            "Do not retry this write/action tool while in ask mode.",
            "Use read-only tools or ask the user to change mode if they want execution.",
        ]),
        _ if input.code.equals_str("permission_required") => {
            texts(&["Pause tool execution and ask the user for approval with a concise reason."])
        }
        _ if input.code.equals_str("permission_denied")
            || input.code.equals_str("permission_cancelled") =>
        {
            texts(&[
                "Do not retry the same action. Choose a lower-privilege alternative or ask the user how to proceed.",
            ])
        }
        _ if input.code.equals_str("path_not_allowed") => vec![
            wrap("Use a path inside the workspace root: ", &input.cwd, "."),
            "If the outside path is essential, explain why and ask the user for access.".into(),
        ],
        _ if input.code.equals_str("file_not_found") => {
            texts(&["Verify the path with glob/grep/read-only inspection before retrying."])
        }
        _ if input.code.equals_str("result_too_large") => {
            texts(&["Reduce the requested scope and fetch a smaller result."])
        }
        _ if input.code.equals_str("tool_timeout") => {
            texts(&["Break the operation into smaller steps or retry with a narrower scope."])
        }
        _ if input.code.equals_str("setup_required") => {
            texts(&["Tell the user what configuration is missing and wait for it to be provided."])
        }
        _ if input.code.equals_str("unsupported_tool") => texts(&[
            "Switch to another available tool or explain that this capability is not configured.",
        ]),
        _ => vec![wrap(
            "Inspect the evidence, change the approach, then retry only with corrected inputs. Current permission mode: ",
            &input.permission_mode,
            ".",
        )],
    }
}
fn avoid(code: &JsString) -> Option<&'static str> {
    [
        (
            "permission_required",
            "This tool requires user approval; repeated calls cannot grant approval.",
        ),
        (
            "permission_denied",
            "The action was denied by policy or a hook.",
        ),
        ("permission_cancelled", "The user cancelled this action."),
        (
            "path_not_allowed",
            "The path policy will continue blocking this location.",
        ),
        (
            "setup_required",
            "The missing setup must be completed outside this tool call.",
        ),
        (
            "tool_unavailable",
            "This known tool is unavailable; repeated calls cannot enable it.",
        ),
        (
            "plan_mode_violation",
            "Plan mode blocks this class of tool until execution mode is restored.",
        ),
        (
            "ask_mode_violation",
            "Ask mode blocks this class of tool until execution mode is restored.",
        ),
    ]
    .into_iter()
    .find_map(|(c, s)| code.equals_str(c).then_some(s))
}
fn original(message: &JsString) -> Option<JsString> {
    let filtered = lines(message)
        .into_iter()
        .map(|s| trim(&s, false))
        .filter(|s| !prefix_error(&trim(s, true)))
        .collect::<Vec<_>>();
    let cleaned = trim(&join(&filtered, "\n"), true);
    if cleaned.is_empty() {
        return None;
    }
    if cleaned.units().len() <= 2000 {
        Some(cleaned)
    } else {
        Some(cat(&[
            trim(
                &JsString::from_utf16(cleaned.units()[..2000].to_vec()),
                false,
            ),
            format!(
                "\n... [original error truncated; {} chars total]",
                cleaned.units().len()
            )
            .into(),
        ]))
    }
}
pub(crate) fn build(
    input: &ToolErrorRecoveryInput,
) -> Result<ToolErrorRecoveryResult, ProjectionError> {
    let evidence = evidence(input)?;
    let summary = summary(input, &evidence);
    let class = classify(input);
    let next = unique(actions(input))
        .into_iter()
        .take(3)
        .collect::<Vec<_>>();
    let salient = unique(evidence).into_iter().take(2).collect::<Vec<_>>();
    let avoidance = avoid(&input.code);
    let original = original(&input.message);
    let mut advice = JsObject::new();
    advice.insert("summary".into(), JsValue::String(summary.clone()));
    advice.insert("failureClass".into(), JsValue::from(class));
    advice.insert(
        "nextActions".into(),
        JsValue::Array(next.iter().cloned().map(JsValue::String).collect()),
    );
    advice.insert(
        "salientEvidence".into(),
        JsValue::Array(salient.iter().cloned().map(JsValue::String).collect()),
    );
    if let Some(s) = avoidance {
        advice.insert("avoidRetryReason".into(), s.into());
    }
    if let Some(s) = &original {
        advice.insert("originalError".into(), JsValue::String(s.clone()));
    }
    let mut lines = vec![
        cat(&[
            "TOOL_ERROR[".into(),
            input.code.clone(),
            "][".into(),
            input.tool_name.clone(),
            "][".into(),
            class.into(),
            "]".into(),
        ]),
        wrap("Summary: ", &summary, ""),
    ];
    if !salient.is_empty() {
        lines.push("Evidence:".into());
        lines.extend(salient.iter().map(|s| wrap("- ", s, "")));
    }
    if let Some(s) = avoidance {
        lines.push(format!("Do not retry unchanged: {s}").into());
    }
    if let Some(s) = original {
        lines.extend(["Original error:".into(), s]);
    }
    if !next.is_empty() {
        lines.push("Next actions:".into());
        lines.extend(
            next.iter()
                .enumerate()
                .map(|(i, s)| wrap(&format!("{}. ", i + 1), s, "")),
        );
    }
    Ok(ToolErrorRecoveryResult {
        message: join(&lines, "\n"),
        advice: BoundarySnapshot::capture(&JsValue::Object(advice)),
    })
}
