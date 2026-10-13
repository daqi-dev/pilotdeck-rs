//! Source-derived controls for Original errorRecovery.ts, not full AgentLoop acceptance.
use pilotdeck_agent::tool_error_recovery::{
    ToolErrorRecoveryInput, ToolErrorRecoveryResult, build_tool_error_recovery,
};
use pilotdeck_protocol::{JsObject, JsString, JsValue, model::BoundarySnapshot};
fn object(fields: &[(&str, JsValue)]) -> JsValue {
    let mut o = JsObject::new();
    for (k, v) in fields {
        o.insert((*k).into(), v.clone());
    }
    JsValue::Object(o)
}
fn run(
    code: &str,
    tool: &str,
    message: impl Into<JsString>,
    details: Option<JsValue>,
) -> ToolErrorRecoveryResult {
    build_tool_error_recovery(&ToolErrorRecoveryInput {
        code: code.into(),
        tool_name: tool.into(),
        message: message.into(),
        cwd: "/fictional/work".into(),
        permission_mode: "ask".into(),
        details: details.as_ref().map(BoundarySnapshot::capture),
    })
    .unwrap()
}
fn value<'a>(r: &'a ToolErrorRecoveryResult, k: &str) -> &'a JsValue {
    r.advice.value().get(k).unwrap()
}
fn text(r: &ToolErrorRecoveryResult, k: &str) -> JsString {
    value(r, k).as_string().unwrap().clone()
}
fn list(r: &ToolErrorRecoveryResult, k: &str) -> Vec<JsString> {
    match value(r, k) {
        JsValue::Array(v) => v.iter().map(|x| x.as_string().unwrap().clone()).collect(),
        _ => panic!("not array"),
    }
}
fn issue(path: JsValue, message: JsValue, code: JsValue) -> JsValue {
    object(&[("path", path), ("message", message), ("code", code)])
}
fn details_issues(v: Vec<JsValue>) -> JsValue {
    object(&[("issues", JsValue::Array(v))])
}
#[test]
fn seventeen_codes_classify_before_generic_haystack() {
    for (code, class) in [
        ("tool_not_found", "switch_tool"),
        ("tool_unavailable", "tool_unavailable"),
        ("invalid_tool_input", "fix_input"),
        ("permission_denied", "ask_user"),
        ("permission_cancelled", "ask_user"),
        ("permission_required", "ask_user"),
        ("tool_execution_failed", "ask_user"),
        ("tool_aborted", "retry_later"),
        ("tool_timeout", "retry_later"),
        ("result_too_large", "reduce_scope"),
        ("path_not_allowed", "ask_user"),
        ("file_not_found", "fix_input"),
        ("file_conflict", "fix_input"),
        ("unsupported_tool", "switch_tool"),
        ("setup_required", "ask_user"),
        ("plan_mode_violation", "switch_tool"),
        ("ask_mode_violation", "switch_tool"),
    ] {
        assert_eq!(
            text(&run(code, "bash", "EACCES", None), "failureClass"),
            JsString::from(class),
            "{code}"
        );
    }
}
#[test]
fn own_advice_order_and_optional_absence() {
    let r = run("tool_execution_failed", "x", "", None);
    let keys = r
        .advice
        .value()
        .as_object()
        .unwrap()
        .entries()
        .iter()
        .map(|(k, _)| k.clone())
        .collect::<Vec<_>>();
    assert_eq!(
        keys,
        vec![
            "summary".into(),
            "failureClass".into(),
            "nextActions".into(),
            "salientEvidence".into()
        ]
    );
    assert_eq!(
        text(&r, "summary"),
        JsString::from("x failed with tool_execution_failed.")
    );
    assert!(list(&r, "salientEvidence").is_empty());
}
#[test]
fn exact_message_assembly_order() {
    let r = run("permission_required", "x", "reason", None);
    assert_eq!(
        r.message,
        JsString::from(
            "TOOL_ERROR[permission_required][x][ask_user]\nSummary: reason\nEvidence:\n- reason\nDo not retry unchanged: This tool requires user approval; repeated calls cannot grant approval.\nOriginal error:\nreason\nNext actions:\n1. Pause tool execution and ask the user for approval with a concise reason."
        )
    );
}
#[test]
fn special_summary_does_not_use_evidence() {
    assert_eq!(
        text(&run("tool_not_found", "ghost", "EACCES", None), "summary"),
        JsString::from("The model emitted a tool name that is not registered: ghost.")
    );
}
#[test]
fn issue_priority_required_beats_earlier_invalid_type() {
    let d = details_issues(vec![
        issue("$.kind".into(), "wrong kind".into(), "invalid_type".into()),
        issue(
            "$.content".into(),
            "required content".into(),
            "required".into(),
        ),
    ]);
    let r = run("invalid_tool_input", "write_file", "", Some(d));
    assert_eq!(text(&r, "summary"), JsString::from("wrong kind"));
    assert_eq!(list(&r,"nextActions"),vec!["Include the required parameter `content` in the next write_file call.".into(),"Create a smaller but complete draft first, then continue with focused follow-up edits or patches.".into()]);
}
#[test]
fn issue_defaults_arrays_and_unknown_code() {
    let d = details_issues(vec![
        JsValue::Null,
        JsValue::Bool(false),
        JsValue::Number(2.0),
        JsValue::Array(vec![]),
        issue("$".into(), "bad".into(), "future".into()),
    ]);
    let r = run("invalid_tool_input", "x", "", Some(d));
    assert_eq!(text(&r, "summary"), JsString::from("Invalid tool input."));
    assert_eq!(
        list(&r, "salientEvidence"),
        vec!["input: Invalid tool input.".into(), "input: bad".into()]
    );
    assert_eq!(
        list(&r, "nextActions"),
        vec!["Fix `input`: Invalid tool input.".into()]
    );
}
#[test]
fn empty_issue_message_falls_back_but_whitespace_is_truthy() {
    let d = details_issues(vec![issue("$.p".into(), "".into(), "required".into())]);
    assert_eq!(
        text(&run("invalid_tool_input", "x", "raw", Some(d)), "summary"),
        JsString::from("raw")
    );
    let d = details_issues(vec![issue(
        "$.p".into(),
        "\u{feff} ".into(),
        "required".into(),
    )]);
    assert!(text(&run("invalid_tool_input", "x", "raw", Some(d)), "summary").is_empty());
}
#[test]
fn invalid_actions_file_snapshot_before_schema() {
    let d = details_issues(vec![issue(
        "$.p".into(),
        "required".into(),
        "required".into(),
    )]);
    let r = run(
        "invalid_tool_input",
        "write_file",
        "File has not been read yet",
        Some(d),
    );
    assert_eq!(
        list(&r, "nextActions")[0],
        JsString::from(
            "Call read_file with the same file_path to establish the current file snapshot before writing."
        )
    );
}
#[test]
fn invalid_actions_changed_file() {
    let r = run(
        "invalid_tool_input",
        "write_file",
        "File has changed since the last read",
        None,
    );
    assert_eq!(
        list(&r, "nextActions")[0],
        JsString::from(
            "Call read_file with the same file_path again to refresh the file snapshot."
        )
    );
}
#[test]
fn dot_regex_does_not_cross_line_terminators() {
    for separator in ["\n", "\r", "\u{2028}", "\u{2029}"] {
        let r = run(
            "invalid_tool_input",
            "edit_file",
            format!("old_string{separator}not found"),
            None,
        );
        assert_eq!(list(&r, "nextActions").len(), 1);
    }
    let r = run(
        "invalid_tool_input",
        "edit_file",
        "old_string abc not found",
        None,
    );
    assert_eq!(
        list(&r, "nextActions")[0],
        JsString::from(
            "Call read_file with the same file_path and copy the exact current text you need to replace."
        )
    );
}
#[test]
fn ascii_digit_regex_and_multiple_match_action() {
    let r = run(
        "invalid_tool_input",
        "edit_file",
        "Found 12 matches of old_string",
        None,
    );
    assert_eq!(list(&r, "nextActions").len(), 2);
    let r = run(
        "invalid_tool_input",
        "edit_file",
        "Found １２ matches of old_string",
        None,
    );
    assert_eq!(list(&r, "nextActions").len(), 1);
}
#[test]
fn bash_timeout_precedes_background_and_retains_three_actions() {
    let r = run(
        "invalid_tool_input",
        "bash",
        "timeout 900000ms exceeds maximum background",
        None,
    );
    let a = list(&r, "nextActions");
    assert_eq!(a.len(), 3);
    assert_eq!(
        a[0],
        JsString::from("Use timeout=600000 or less for foreground bash.")
    );
    let r = run(
        "invalid_tool_input",
        "not_bash",
        "background task_wait",
        None,
    );
    assert_eq!(list(&r, "nextActions").len(), 1);
}
#[test]
fn all_schema_action_shapes() {
    for (code, expected) in [
        (
            "invalid_type",
            "Change `p` to the expected type before retrying: wrong.",
        ),
        (
            "unknown_property",
            "Remove or rename the unexpected parameter `p`; use only parameters from the tool schema.",
        ),
        (
            "invalid_enum",
            "Use one of the allowed values for `p`: wrong.",
        ),
        ("invalid_schema", "Fix `p`: wrong"),
    ] {
        let d = details_issues(vec![issue("$.p".into(), "wrong".into(), code.into())]);
        assert_eq!(
            list(&run("invalid_tool_input", "x", "", Some(d)), "nextActions")[0],
            JsString::from(expected)
        );
    }
}
#[test]
fn clean_path_only_first_dollar_optional_dot() {
    let d = details_issues(vec![issue(
        "$.$.p".into(),
        "bad".into(),
        "invalid_schema".into(),
    )]);
    assert_eq!(
        list(
            &run("invalid_tool_input", "x", "", Some(d)),
            "salientEvidence"
        )[0],
        JsString::from("$.p: bad")
    );
}
#[test]
fn web_fetch_status_groups_and_secondary_stage() {
    for (status, class) in [
        (401.0, "ask_user"),
        (403.0, "ask_user"),
        (404.0, "fix_input"),
        (410.0, "fix_input"),
        (408.0, "retry_later"),
        (429.0, "retry_later"),
        (500.0, "retry_later"),
        (200.0, "environment_issue"),
    ] {
        let d = object(&[
            ("stage", "http_fetch".into()),
            ("status", JsValue::Number(status)),
        ]);
        assert_eq!(
            text(
                &run("permission_denied", "web_fetch", "", Some(d)),
                "failureClass"
            ),
            JsString::from(class)
        );
    }
    let d = object(&[("stage", "secondary_model".into())]);
    assert_eq!(
        list(
            &run("tool_execution_failed", "web_fetch", "", Some(d)),
            "nextActions"
        )[0],
        JsString::from(
            "Retry with mode \"raw\" to inspect the fetched markdown without secondary model processing."
        )
    );
}
#[test]
fn web_timeout_class_does_not_change_status_actions() {
    let d = object(&[
        ("stage", "http_fetch".into()),
        ("status", JsValue::Number(200.0)),
    ]);
    let r = run(
        "tool_execution_failed",
        "web_fetch",
        "network timeout",
        Some(d),
    );
    assert_eq!(text(&r, "failureClass"), JsString::from("retry_later"));
    assert_eq!(
        list(&r, "nextActions")[0],
        JsString::from(
            "Inspect the HTTP status and try a different source or access path before retrying."
        )
    );
}
#[test]
fn web_invalid_input_overrides_stage() {
    let d = object(&[("stage", "secondary_model".into())]);
    let r = run("invalid_tool_input", "web_fetch", "bad", Some(d));
    assert_eq!(text(&r, "failureClass"), JsString::from("fix_input"));
    assert_eq!(
        list(&r, "nextActions")[0],
        JsString::from(
            "Fix the specific invalid argument described in the error message before calling the tool again."
        )
    );
}
#[test]
fn details_haystack_is_strings_only_and_permission_precedes_type() {
    let d = object(&[
        ("stderr", "TypeError EACCES".into()),
        ("stdout", JsValue::Number(3.0)),
    ]);
    assert_eq!(
        text(
            &run("tool_execution_failed", "x", "timed out", Some(d)),
            "failureClass"
        ),
        JsString::from("ask_user")
    );
    let d = object(&[
        ("stderr", JsValue::Bool(true)),
        ("message", "ReferenceError".into()),
    ]);
    assert_eq!(
        text(
            &run("tool_execution_failed", "x", "", Some(d)),
            "failureClass"
        ),
        JsString::from("fix_input")
    );
}
#[test]
fn legacy_regex_i_does_not_unicode_fold_kelvin() {
    assert_eq!(
        text(
            &run("tool_execution_failed", "x", "disK quota", None),
            "failureClass"
        ),
        JsString::from("environment_issue")
    );
    assert_eq!(
        text(
            &run("invalid_tool_input", "bash", "taſk_wait", None),
            "failureClass"
        ),
        JsString::from("fix_input")
    );
    assert_eq!(
        list(
            &run("invalid_tool_input", "bash", "taſk_wait", None),
            "nextActions"
        )
        .len(),
        1
    );
}
#[test]
fn evidence_first_two_lines_are_sliced_before_dedup() {
    let d = object(&[("status", JsValue::Number(404.0))]);
    let r = run("tool_execution_failed", "x", "same\nsame\nthird", Some(d));
    assert_eq!(
        list(&r, "salientEvidence"),
        vec!["same".into(), "HTTP status: 404".into()]
    );
}
#[test]
fn number_evidence_preserves_nan_infinity_and_negative_zero() {
    for (n, expected) in [
        (f64::NAN, "HTTP status: NaN"),
        (f64::INFINITY, "HTTP status: Infinity"),
        (f64::NEG_INFINITY, "HTTP status: -Infinity"),
        (-0.0, "HTTP status: 0"),
    ] {
        let d = object(&[("status", JsValue::Number(n))]);
        assert_eq!(
            list(
                &run("tool_execution_failed", "x", "", Some(d)),
                "salientEvidence"
            )[0],
            JsString::from(expected)
        );
    }
}
#[test]
fn whitespace_ecmascript_feff_included_nel_excluded() {
    let r = run(
        "tool_execution_failed",
        "x",
        "\u{feff}a\u{00a0}\t b\u{feff}",
        None,
    );
    assert_eq!(text(&r, "summary"), JsString::from("a b"));
    let r = run("tool_execution_failed", "x", "\u{0085}a\u{0085}", None);
    assert_eq!(text(&r, "summary"), JsString::from("\u{0085}a\u{0085}"));
}
#[test]
fn summary_220_and217_utf16_can_split_surrogate() {
    let mut units = vec![97; 216];
    units.extend([0xd83d, 0xde00]);
    units.extend([98; 3]);
    let r = run(
        "tool_execution_failed",
        "x",
        JsString::from_utf16(units),
        None,
    );
    let mut expected = vec![97; 216];
    expected.extend([0xd83d, 46, 46, 46]);
    assert_eq!(text(&r, "summary").units(), expected);
    assert_eq!(
        text(
            &run("tool_execution_failed", "x", "a".repeat(220), None),
            "summary"
        )
        .units()
        .len(),
        220
    );
}
#[test]
fn original_header_filter_crlf_and_lone_cr_preserved() {
    let r = run(
        "tool_execution_failed",
        "x",
        " TOOL_ERROR[old] \r\n first \r\nsecond\rinside \n",
        None,
    );
    assert_eq!(
        text(&r, "originalError"),
        JsString::from("first\nsecond\rinside")
    );
}
#[test]
fn original_2000_utf16_boundary_and_total_count() {
    let mut units = vec![97; 1999];
    units.extend([0xd83d, 0xde00]);
    let r = run(
        "tool_execution_failed",
        "x",
        JsString::from_utf16(units),
        None,
    );
    let mut expected = vec![97; 1999];
    expected.push(0xd83d);
    expected.extend("\n... [original error truncated; 2001 chars total]".encode_utf16());
    assert_eq!(text(&r, "originalError").units(), expected);
    assert_eq!(
        text(
            &run("tool_execution_failed", "x", "a".repeat(2000), None),
            "originalError"
        )
        .units()
        .len(),
        2000
    );
}
#[test]
fn raw_details_null_undefined_and_unknown_extensions_are_not_policy_mutated() {
    let d = object(&[
        ("issues", JsValue::Undefined),
        ("extension", object(&[("unknown", JsValue::Null)])),
    ]);
    let before = d.clone();
    let input = ToolErrorRecoveryInput {
        code: "path_not_allowed".into(),
        tool_name: "x".into(),
        message: "".into(),
        cwd: JsString::from_utf16(vec![0xd800]),
        permission_mode: "ask".into(),
        details: Some(BoundarySnapshot::capture(&d)),
    };
    let r = build_tool_error_recovery(&input).unwrap();
    assert_eq!(input.details.unwrap().value(), &before);
    assert_eq!(
        list(&r, "nextActions")[0]
            .units()
            .iter()
            .filter(|u| **u == 0xd800)
            .count(),
        1
    );
}
