//! Fixed original LargeFileRepair.ts policy. No filesystem or permission authority.
use crate::boundary_match::same_value;
use crate::{
    LargeFileRepair, LargeFileRepairDecision, LargeFileRepairToolContext, ProjectionError,
    RepairStrip, ToolExecutionResult, ToolResultDisposition,
};
use pilotdeck_protocol::model::{BoundarySnapshot, FinishReason, ToolCall};
use pilotdeck_protocol::{JsString, JsValue, stringify_json};

const PRE_LIMIT: u32 = 5;
const POST_LIMIT: u32 = 5;
const TRUNCATION_LIMIT: u32 = 10;
const INTERNAL_HINT: &str =
    "INTERNAL RECOVERY HINT - not a user request. Continue the original task.";

pub(crate) fn on_invalid_tool_input(s: &mut LargeFileRepair) -> Option<LargeFileRepairDecision> {
    if !s.pending_large_file_repair {
        return None;
    }
    Some(if s.wrote_file {
        post_draft(s, "large_file_invalid_input_after_write")
    } else {
        pre_draft(s, "large_file_invalid_input", RepairStrip::ErrorPair)
    })
}
pub(crate) fn on_no_tool_calls(s: &mut LargeFileRepair) -> Option<LargeFileRepairDecision> {
    if !s.pending_large_file_repair || s.wrote_file {
        return None;
    }
    Some(pre_draft(
        s,
        "large_file_no_tool_call",
        RepairStrip::Assistant,
    ))
}
pub(crate) fn recover_from_repaired_truncation(
    s: &mut LargeFileRepair,
    calls: &[ToolCall],
) -> Option<LargeFileRepairDecision> {
    truncation_decision(s, calls.iter().any(|call| is_write_name(&call.name)))
}

/// Source evaluates Array.some before testing the independent truncation cap.
/// The name-only boundary must not require a call ID or fabricate its input.
pub(crate) fn recover_from_boundary_truncation(
    s: &mut LargeFileRepair,
    calls: &[BoundarySnapshot],
) -> Result<Option<LargeFileRepairDecision>, ProjectionError> {
    let mut has_write = false;
    for call in calls {
        if is_write(property(Some(call.value()), "name")?) {
            has_write = true;
            break;
        }
    }
    Ok(truncation_decision(s, has_write))
}

fn truncation_decision(
    s: &mut LargeFileRepair,
    has_write: bool,
) -> Option<LargeFileRepairDecision> {
    if !has_write || s.truncation_recoveries >= TRUNCATION_LIMIT {
        return None;
    }
    s.truncation_recoveries += 1;
    s.pending_large_file_repair = true;
    Some(LargeFileRepairDecision::Continue {
        purpose: if s.wrote_file {
            "large_file_repaired_truncation_after_write"
        } else {
            "large_file_repaired_truncation"
        }
        .into(),
        strip: Some(RepairStrip::Assistant),
        prompt: if s.wrote_file {
            post_prompt(&s.recent_file_paths, s.post_draft_attempts)
        } else {
            pre_prompt(s.pre_draft_attempts + 1)
        },
    })
}
fn pre_draft(
    s: &mut LargeFileRepair,
    purpose: &str,
    strip: RepairStrip,
) -> LargeFileRepairDecision {
    if s.pre_draft_attempts >= PRE_LIMIT {
        return LargeFileRepairDecision::Stop {
            reason: format!(
                "Large file repair failed before any workspace file was created after {} attempts.",
                s.pre_draft_attempts
            )
            .into(),
        };
    }
    s.pre_draft_attempts += 1;
    LargeFileRepairDecision::Continue {
        prompt: pre_prompt(s.pre_draft_attempts),
        purpose: purpose.into(),
        strip: Some(strip),
    }
}
fn post_draft(s: &mut LargeFileRepair, purpose: &str) -> LargeFileRepairDecision {
    if s.post_draft_attempts >= POST_LIMIT {
        return LargeFileRepairDecision::Stop { reason: format!("Large file repair stopped after {} post-draft attempts. A workspace file already exists; report the current file path and remaining gap.",s.post_draft_attempts).into() };
    }
    s.post_draft_attempts += 1;
    LargeFileRepairDecision::Continue {
        prompt: post_prompt(&s.recent_file_paths, s.post_draft_attempts),
        purpose: purpose.into(),
        strip: None,
    }
}
fn joined(parts: &[JsString], separator: &str) -> JsString {
    let separator: Vec<_> = separator.encode_utf16().collect();
    let mut out = Vec::new();
    for (i, p) in parts.iter().enumerate() {
        if i > 0 {
            out.extend_from_slice(&separator);
        }
        out.extend_from_slice(p.units());
    }
    JsString::from_utf16(out)
}
fn filtered_lines(parts: Vec<JsString>) -> JsString {
    joined(
        &parts
            .into_iter()
            .filter(|p| !p.is_empty())
            .collect::<Vec<_>>(),
        "\n",
    )
}
fn pre_prompt(attempt: u32) -> JsString {
    let max_lines = if attempt <= 2 {
        80
    } else if attempt <= 4 {
        40
    } else {
        20
    };
    filtered_lines(vec![
        INTERNAL_HINT.into(),
        format!("The previous file write/edit attempt appears incomplete or invalid, likely because the model output was truncated. Recovery attempt {attempt}/{PRE_LIMIT}.").into(),
        format!("Use one small direct file tool call. Keep write_file content under {max_lines} lines.").into(),
        "Recovery guidance:".into(),
        "1. Continue the original task; do not treat this hint as a new user request.".into(),
        "2. Provide the required tool parameters such as file_path and content.".into(),
        "3. Create a minimal structurally valid skeleton first:".into(),
        "   - HTML: doctype, head, minimal style, body with one section, closing tags.".into(),
        "   - Code: imports, one function/class stub, exports if needed.".into(),
        "   - Prose/report: title, first paragraph, and a clear continuation marker.".into(),
        "4. After a file exists, extend it incrementally with focused edit_file calls.".into(),
        "5. Do not attempt to generate the complete artifact in one oversized call.".into(),
        "Prefer write_file for the initial skeleton. Avoid shell heredocs or echo for this recovery step.".into(),
        if attempt >= PRE_LIMIT { "Final recovery attempt: write the smallest viable skeleton, even if it only establishes the file structure." } else { "" }.into(),
    ])
}
fn post_prompt(paths: &[JsString], attempt: u32) -> JsString {
    let file_text = if paths.is_empty() {
        "A workspace draft already exists.".into()
    } else {
        joined(
            &[
                "Recent file path context for orientation only: ".into(),
                joined(paths, ", "),
                ".".into(),
            ],
            "",
        )
    };
    filtered_lines(vec![
        INTERNAL_HINT.into(),file_text,
        format!("Post-draft recovery attempt {attempt}/{POST_LIMIT}: continue the original task from the current workspace state.").into(),
        "Recovery guidance:".into(),
        "1. If current content matters, inspect the file first with read_file.".into(),
        "2. Add or replace one small section at a time with edit_file; keep new content under 60 lines.".into(),
        "3. Do not rewrite the whole file or regenerate from scratch.".into(),
        "4. If you just wrote a script or generator, run it and verify the produced output before asking for clarification.".into(),
        "5. Ask the user only for a real blocker that cannot be resolved from the workspace or the original task.".into(),
        "Use a focused edit_file replacement or insertion point such as a continuation marker when available.".into(),
        if attempt >= POST_LIMIT { "Final recovery attempt: make one focused edit, run/verify if applicable, or explain the concrete blocker." } else { "" }.into(),
    ])
}
fn is_write_name(name: &JsString) -> bool {
    name.equals_str("write_file") || name.equals_str("edit_file")
}
fn is_write(value: Option<&JsValue>) -> bool {
    value
        .and_then(JsValue::as_string)
        .is_some_and(is_write_name)
}
fn is_string(value: Option<&JsValue>, wanted: &str) -> bool {
    value
        .and_then(JsValue::as_string)
        .is_some_and(|s| s.equals_str(wanted))
}
fn invalid(message: &str) -> ProjectionError {
    ProjectionError::InvalidBoundary {
        message: message.into(),
    }
}
fn property<'a>(
    value: Option<&'a JsValue>,
    name: &str,
) -> Result<Option<&'a JsValue>, ProjectionError> {
    match value {
        None | Some(JsValue::Undefined | JsValue::Null) => {
            let receiver = if matches!(value, Some(JsValue::Null)) {
                "null"
            } else {
                "undefined"
            };
            Err(ProjectionError::SourceTypeError {
                message: format!("Cannot read properties of {receiver} (reading '{name}')").into(),
            })
        }
        Some(value) => Ok(value.get(name)),
    }
}
fn record_writes(s: &mut LargeFileRepair, results: &[&JsValue]) -> Result<(), ProjectionError> {
    for result in results {
        if !is_string(property(Some(result), "type")?, "success")
            || !is_write(property(Some(result), "toolName")?)
        {
            continue;
        }
        s.wrote_file = true;
        if let Some(JsValue::Object(data)) = property(Some(result), "data")?
            && let Some(JsValue::String(path)) = data.get("filePath")
            && !path.is_empty()
        {
            s.recent_file_paths.retain(|old| old != path);
            s.recent_file_paths.insert(0, path.clone());
            s.recent_file_paths.truncate(5);
        }
    }
    Ok(())
}
fn required_issue(result: &JsValue) -> Result<bool, ProjectionError> {
    let error = property(Some(result), "error")?;
    let details = property(error, "details")?;
    let issues = match details {
        None | Some(JsValue::Undefined | JsValue::Null) => None,
        _ => property(details, "issues")?,
    };
    Ok(match issues {
        Some(JsValue::Array(a)) => a.iter().any(|issue| {
            issue
                .as_object()
                .is_some_and(|o| is_string(o.get("code"), "required"))
        }),
        _ => false,
    })
}

// Only literal ASCII alternatives occur in the /iu source pattern. JS Unicode
// simple folding additionally maps Kelvin sign to k and long s to s; neither
// dotted nor dotless i folds to ASCII i. Preserve WTF16 without replacement.
fn large_file_error(message: Option<&JsValue>) -> Result<bool, ProjectionError> {
    let text = to_js_string(message)?;
    let folded: Vec<u16> = text
        .units()
        .iter()
        .map(|u| match *u {
            0x41..=0x5a => u + 32,
            0x017f => u16::from(b's'),
            0x212a => u16::from(b'k'),
            _ => *u,
        })
        .collect();
    Ok([
        "output token",
        "truncated",
        "too large",
        "large file",
        "large artifact",
        "max_output",
        "missing required parameter `content`",
        "required parameter `content` is missing",
    ]
    .iter()
    .any(|pattern| {
        let p: Vec<_> = pattern.encode_utf16().collect();
        folded.windows(p.len()).any(|w| w == p)
    }))
}
fn to_js_string(value: Option<&JsValue>) -> Result<JsString, ProjectionError> {
    enum Part<'a> {
        Value(Option<&'a JsValue>, bool),
        Comma,
    }
    let mut pending = vec![Part::Value(value, false)];
    let mut out = Vec::new();
    while let Some(part) = pending.pop() {
        let (value, in_array) = match part {
            Part::Comma => {
                out.push(u16::from(b','));
                continue;
            }
            Part::Value(v, a) => (v, a),
        };
        let text: JsString = match value {
            None | Some(JsValue::Undefined) if in_array => continue,
            Some(JsValue::Null) if in_array => continue,
            None | Some(JsValue::Undefined) => "undefined".into(),
            Some(JsValue::Null) => "null".into(),
            Some(JsValue::Bool(b)) => if *b { "true" } else { "false" }.into(),
            Some(JsValue::String(s)) => s.clone(),
            Some(JsValue::Number(n)) if n.is_nan() => "NaN".into(),
            Some(JsValue::Number(n)) if *n == f64::INFINITY => "Infinity".into(),
            Some(JsValue::Number(n)) if *n == f64::NEG_INFINITY => "-Infinity".into(),
            Some(v @ JsValue::Number(_)) => stringify_json(v)
                .map_err(|_| invalid("Number ToString failed"))?
                .ok_or_else(|| invalid("Number ToString returned Undefined"))?
                .into(),
            Some(JsValue::Array(a)) => {
                for (i, v) in a.iter().enumerate().rev() {
                    pending.push(Part::Value(Some(v), true));
                    if i > 0 {
                        pending.push(Part::Comma);
                    }
                }
                continue;
            }
            Some(JsValue::Object(o)) if !o.contains_key("toString") => "[object Object]".into(),
            Some(JsValue::Object(_)) => {
                return Err(ProjectionError::SourceTypeError {
                    message: "Cannot convert object to primitive value".into(),
                });
            }
        };
        out.extend_from_slice(text.units());
    }
    Ok(JsString::from_utf16(out))
}
fn pre_risk(
    results: &[&JsValue],
    context: LargeFileRepairToolContext,
) -> Result<bool, ProjectionError> {
    for result in results {
        if !is_string(property(Some(result), "type")?, "error")
            || !is_write(property(Some(result), "toolName")?)
        {
            continue;
        }
        let required = required_issue(result)?;
        if required
            && (is_string(property(Some(result), "toolName")?, "write_file")
                || context.output_truncated)
        {
            return Ok(true);
        }
        let error = property(Some(result), "error")?;
        if large_file_error(property(error, "message")?)? {
            return Ok(true);
        }
    }
    Ok(false)
}
fn post_risk(
    results: &[&JsValue],
    context: LargeFileRepairToolContext,
    pending: bool,
) -> Result<bool, ProjectionError> {
    let truncation = context.output_truncated
        || context.repaired_tool_calls
        || context.finish_reason == Some(FinishReason::Length);
    for result in results {
        if !is_string(property(Some(result), "type")?, "error")
            || !is_write(property(Some(result), "toolName")?)
        {
            continue;
        }
        let error = property(Some(result), "error")?;
        let code = property(error, "code")?;
        if [
            "permission_denied",
            "permission_required",
            "permission_cancelled",
        ]
        .iter()
        .any(|p| is_string(code, p))
        {
            continue;
        }
        if pending
            || truncation
            || is_string(code, "result_too_large")
            || large_file_error(property(error, "message")?)?
        {
            return Ok(true);
        }
    }
    Ok(false)
}
fn analyze(
    s: &mut LargeFileRepair,
    results: &[&JsValue],
    context: LargeFileRepairToolContext,
) -> Result<Option<LargeFileRepairDecision>, ProjectionError> {
    record_writes(s, results)?;
    if s.wrote_file {
        if !post_risk(results, context, s.pending_large_file_repair)? {
            let mut all_success = true;
            for result in results {
                if !is_string(property(Some(result), "type")?, "success") {
                    all_success = false;
                    break;
                }
            }
            let mut write = false;
            if !all_success {
                for result in results {
                    if is_string(property(Some(result), "type")?, "success")
                        && is_write(property(Some(result), "toolName")?)
                    {
                        write = true;
                        break;
                    }
                }
            }
            if all_success || write {
                s.pending_large_file_repair = false;
            }
            return Ok(None);
        }
        s.pending_large_file_repair = true;
        return Ok(Some(post_draft(s, "large_file_post_draft_repair")));
    }
    if pre_risk(results, context)? {
        s.pending_large_file_repair = true;
        return Ok(Some(pre_draft(
            s,
            "large_file_pre_draft_repair",
            RepairStrip::ErrorPair,
        )));
    }
    Ok(None)
}
pub(crate) fn analyze_boundary_results(
    s: &mut LargeFileRepair,
    results: &[BoundarySnapshot],
    context: LargeFileRepairToolContext,
) -> Result<Option<LargeFileRepairDecision>, ProjectionError> {
    analyze(
        s,
        &results
            .iter()
            .map(BoundarySnapshot::value)
            .collect::<Vec<_>>(),
        context,
    )
}
pub(crate) fn analyze_tool_results(
    s: &mut LargeFileRepair,
    results: &[ToolExecutionResult],
    context: LargeFileRepairToolContext,
) -> Result<Option<LargeFileRepairDecision>, ProjectionError> {
    // Validate the policy-relevant normalized projection before consuming echoes.
    // Complete captures supply source details/coercion without becoming policy state.
    for result in results {
        let raw = result.boundary.value();
        let kind = match &result.disposition {
            ToolResultDisposition::Success { .. } => "success",
            ToolResultDisposition::Error { .. } => "error",
        };
        if !is_string(raw.get("type"), kind)
            || raw.get("toolName").and_then(JsValue::as_string) != Some(&result.tool_name)
        {
            return Err(invalid("Tool result contradicts repair policy admission"));
        }
        match &result.disposition {
            ToolResultDisposition::Success { data } => {
                let raw_data = raw.get("data");
                let matches = match data {
                    None => matches!(raw_data, None | Some(JsValue::Undefined)),
                    Some(data) => raw_data.is_some_and(|v| same_value(v, data)),
                };
                if !matches {
                    return Err(invalid(
                        "Tool result data contradicts repair policy admission",
                    ));
                }
            }
            ToolResultDisposition::Error {
                code,
                message,
                fields,
            } => {
                let error = raw
                    .get("error")
                    .and_then(JsValue::as_object)
                    .ok_or_else(|| invalid("Tool error contradicts repair policy admission"))?;
                if error.get("code").and_then(JsValue::as_string) != Some(code)
                    || error.get("message").and_then(JsValue::as_string) != Some(message)
                    || fields.entries().iter().any(|(key, value)| {
                        !error.get_key(key).is_some_and(|v| same_value(v, value))
                    })
                {
                    return Err(invalid("Tool error contradicts repair policy admission"));
                }
            }
        }
    }
    analyze(
        s,
        &results
            .iter()
            .map(|r| r.boundary.value())
            .collect::<Vec<_>>(),
        context,
    )
}
