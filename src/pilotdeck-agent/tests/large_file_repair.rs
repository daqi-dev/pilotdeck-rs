//! Source-derived supplementary author tests. Runtime original oracle remains independent.
use pilotdeck_agent::{
    LargeFileRepair, LargeFileRepairDecision, LargeFileRepairToolContext, ProjectionError,
    RepairStrip, ToolExecutionResult, ToolResultDisposition,
};
use pilotdeck_protocol::model::{BoundarySnapshot, FinishReason, ToolCall};
use pilotdeck_protocol::{JsString, JsValue, parse_json};

const ORDINARY: LargeFileRepairToolContext = LargeFileRepairToolContext {
    output_truncated: false,
    repaired_tool_calls: false,
    finish_reason: Some(FinishReason::ToolCall),
};
fn snap(raw: &str) -> BoundarySnapshot {
    BoundarySnapshot::capture(&parse_json(raw).unwrap())
}
fn raw(
    s: &mut LargeFileRepair,
    input: &str,
    context: LargeFileRepairToolContext,
) -> Option<LargeFileRepairDecision> {
    let value = parse_json(input).unwrap();
    let snapshots = match &value {
        JsValue::Array(a) => a.iter().map(BoundarySnapshot::capture).collect::<Vec<_>>(),
        _ => panic!(),
    };
    s.analyze_boundary_results(&snapshots, context).unwrap()
}
fn continued(
    decision: Option<LargeFileRepairDecision>,
) -> (JsString, JsString, Option<RepairStrip>) {
    match decision.unwrap() {
        LargeFileRepairDecision::Continue {
            prompt,
            purpose,
            strip,
        } => (prompt, purpose, strip),
        x => panic!("{x:?}"),
    }
}
fn has(value: &JsString, text: &str) -> bool {
    let text: Vec<_> = text.encode_utf16().collect();
    value.units().windows(text.len()).any(|w| w == text)
}
fn call(name: &str) -> ToolCall {
    ToolCall {
        timeline: None,
        id: "call".into(),
        name: name.into(),
        input: JsValue::Null,
        raw: None,
    }
}
fn error() -> &'static str {
    r#"[{"type":"error","toolName":"write_file","error":{"message":"required parameter `content` is missing","code":"invalid_tool_input"}}]"#
}

#[test]
fn ordinary_edits_after_write_do_not_start_recovery_original_regression() {
    let mut s = LargeFileRepair::new();
    assert_eq!(s.recommended_max_output_tokens(), 16_384.0);
    assert!(s.on_invalid_tool_input().is_none());
    assert!(s.on_no_tool_calls().is_none());
    assert!(raw(&mut s,r#"[{"type":"success","toolName":"write_file","data":{"filePath":"/workspace/report.xlsx"}}]"#,ORDINARY).is_none());
    for _ in 0..6 {
        assert!(raw(&mut s,r#"[{"type":"error","toolName":"edit_file","error":{"message":"String to replace not found in file.","code":"invalid_tool_input"}}]"#,ORDINARY).is_none());
    }
    assert!(!s.has_pending_repair());
}

#[test]
fn five_pre_attempts_keep_exact_line_limits_strips_and_stop_reason() {
    let mut s = LargeFileRepair::new();
    for attempt in 1..=5 {
        let (prompt, purpose, strip) = continued(raw(&mut s, error(), ORDINARY));
        assert_eq!(purpose, JsString::from("large_file_pre_draft_repair"));
        assert_eq!(strip, Some(RepairStrip::ErrorPair));
        assert!(has(&prompt, &format!("Recovery attempt {attempt}/5.")));
        let lines = if attempt <= 2 {
            80
        } else if attempt <= 4 {
            40
        } else {
            20
        };
        assert!(has(&prompt, &format!("under {lines} lines.")));
        assert_eq!(has(&prompt, "Final recovery attempt:"), attempt == 5);
        assert!(!has(&prompt, "\n\n"));
    }
    assert_eq!(
        raw(&mut s, error(), ORDINARY),
        Some(LargeFileRepairDecision::Stop {
            reason:
                "Large file repair failed before any workspace file was created after 5 attempts."
                    .into()
        })
    );
    assert!(s.has_pending_repair());
}

#[test]
fn literal_pre_prompt_matches_original_filter_boolean_output() {
    let mut s = LargeFileRepair::new();
    let (prompt, _, _) = continued(raw(&mut s, error(), ORDINARY));
    let expected = concat!(
        "INTERNAL RECOVERY HINT - not a user request. Continue the original task.\n",
        "The previous file write/edit attempt appears incomplete or invalid, likely because the model output was truncated. Recovery attempt 1/5.\n",
        "Use one small direct file tool call. Keep write_file content under 80 lines.\n",
        "Recovery guidance:\n",
        "1. Continue the original task; do not treat this hint as a new user request.\n",
        "2. Provide the required tool parameters such as file_path and content.\n",
        "3. Create a minimal structurally valid skeleton first:\n",
        "   - HTML: doctype, head, minimal style, body with one section, closing tags.\n",
        "   - Code: imports, one function/class stub, exports if needed.\n",
        "   - Prose/report: title, first paragraph, and a clear continuation marker.\n",
        "4. After a file exists, extend it incrementally with focused edit_file calls.\n",
        "5. Do not attempt to generate the complete artifact in one oversized call.\n",
        "Prefer write_file for the initial skeleton. Avoid shell heredocs or echo for this recovery step."
    );
    assert_eq!(prompt, JsString::from(expected));
}

#[test]
fn five_post_attempts_are_separate_and_stop_preserves_existing_file_reason() {
    let mut s = LargeFileRepair::new();
    for _ in 0..5 {
        raw(&mut s, error(), ORDINARY);
    }
    raw(
        &mut s,
        r#"[{"type":"success","toolName":"write_file","data":{"filePath":"/workspace/a"}}]"#,
        ORDINARY,
    );
    assert!(!s.has_pending_repair());
    for attempt in 1..=5 {
        let (prompt, purpose, strip) = continued(raw(
            &mut s,
            r#"[{"type":"error","toolName":"edit_file","error":{"code":"result_too_large","message":"failure"}}]"#,
            ORDINARY,
        ));
        assert_eq!(purpose, JsString::from("large_file_post_draft_repair"));
        assert_eq!(strip, None);
        assert!(has(
            &prompt,
            &format!("Post-draft recovery attempt {attempt}/5:")
        ));
        assert_eq!(has(&prompt, "Final recovery attempt:"), attempt == 5);
    }
    assert!(
        matches!(s.on_invalid_tool_input(),Some(LargeFileRepairDecision::Stop{reason}) if reason.equals_str("Large file repair stopped after 5 post-draft attempts. A workspace file already exists; report the current file path and remaining gap."))
    );
    assert!(s.on_no_tool_calls().is_none());
}

#[test]
fn truncation_budget_ten_does_not_consume_pre_or_post_attempts() {
    let mut s = LargeFileRepair::new();
    assert!(
        s.recover_from_repaired_truncation(&[call("bash")])
            .is_none()
    );
    for _ in 0..10 {
        let (prompt, purpose, strip) =
            continued(s.recover_from_repaired_truncation(&[call("edit_file")]));
        assert_eq!(purpose, JsString::from("large_file_repaired_truncation"));
        assert_eq!(strip, Some(RepairStrip::Assistant));
        assert!(has(&prompt, "Recovery attempt 1/5."));
    }
    assert!(
        s.recover_from_repaired_truncation(&[call("write_file")])
            .is_none()
    );
    let (prompt, purpose, strip) = continued(s.on_no_tool_calls());
    assert!(has(&prompt, "Recovery attempt 1/5."));
    assert_eq!(purpose, JsString::from("large_file_no_tool_call"));
    assert_eq!(strip, Some(RepairStrip::Assistant));
    raw(
        &mut s,
        r#"[{"type":"success","toolName":"edit_file"}]"#,
        ORDINARY,
    );
    assert!(
        s.recover_from_repaired_truncation(&[call("write_file")])
            .is_none()
    );
}

#[test]
fn post_truncation_uses_zero_then_current_post_count_without_increment() {
    let mut s = LargeFileRepair::new();
    raw(
        &mut s,
        r#"[{"type":"success","toolName":"write_file"}]"#,
        ORDINARY,
    );
    let (prompt, purpose, strip) =
        continued(s.recover_from_repaired_truncation(&[call("write_file")]));
    assert_eq!(
        prompt,
        JsString::from(concat!(
            "INTERNAL RECOVERY HINT - not a user request. Continue the original task.\n",
            "A workspace draft already exists.\n",
            "Post-draft recovery attempt 0/5: continue the original task from the current workspace state.\n",
            "Recovery guidance:\n",
            "1. If current content matters, inspect the file first with read_file.\n",
            "2. Add or replace one small section at a time with edit_file; keep new content under 60 lines.\n",
            "3. Do not rewrite the whole file or regenerate from scratch.\n",
            "4. If you just wrote a script or generator, run it and verify the produced output before asking for clarification.\n",
            "5. Ask the user only for a real blocker that cannot be resolved from the workspace or the original task.\n",
            "Use a focused edit_file replacement or insertion point such as a continuation marker when available."
        ))
    );
    assert!(has(&prompt, "Post-draft recovery attempt 0/5:"));
    assert_eq!(
        purpose,
        JsString::from("large_file_repaired_truncation_after_write")
    );
    assert_eq!(strip, Some(RepairStrip::Assistant));
    let (prompt, purpose, strip) = continued(s.on_invalid_tool_input());
    assert!(has(&prompt, "attempt 1/5:"));
    assert_eq!(strip, None);
    assert_eq!(
        purpose,
        JsString::from("large_file_invalid_input_after_write")
    );
    let (prompt, _, _) = continued(s.recover_from_repaired_truncation(&[call("write_file")]));
    assert!(has(&prompt, "attempt 1/5:"));
}

#[test]
fn successful_write_clears_episode_even_with_unrelated_error_without_resetting_counters() {
    let mut s = LargeFileRepair::new();
    raw(&mut s, error(), ORDINARY);
    assert!(s.has_pending_repair());
    assert!(raw(&mut s,r#"[{"type":"success","toolName":"write_file","data":{"filePath":"/workspace/report.xlsx"}},{"type":"error","toolName":"bash","error":null}]"#,ORDINARY).is_none());
    assert!(!s.has_pending_repair());
    let (prompt, _, _) = continued(raw(
        &mut s,
        r#"[{"type":"error","toolName":"edit_file","error":{"code":"result_too_large"}}]"#,
        ORDINARY,
    ));
    assert!(has(&prompt, "Post-draft recovery attempt 1/5:"));
    assert!(raw(&mut s, "[]", ORDINARY).is_none());
    assert!(!s.has_pending_repair());
    let (prompt, _, _) = continued(raw(
        &mut s,
        r#"[{"type":"error","toolName":"edit_file","error":{"code":"result_too_large"}}]"#,
        ORDINARY,
    ));
    assert!(has(&prompt, "Post-draft recovery attempt 2/5:"));
}

#[test]
fn post_draft_permission_codes_always_excluded_even_with_pending_and_truncation() {
    let mut s = LargeFileRepair::new();
    raw(
        &mut s,
        r#"[{"type":"success","toolName":"write_file"}]"#,
        ORDINARY,
    );
    s.recover_from_repaired_truncation(&[call("write_file")]);
    let context = LargeFileRepairToolContext {
        output_truncated: true,
        repaired_tool_calls: true,
        finish_reason: Some(FinishReason::Length),
    };
    for code in [
        "permission_denied",
        "permission_required",
        "permission_cancelled",
    ] {
        let result = format!(
            r#"[{{"type":"error","toolName":"edit_file","error":{{"code":"{code}","message":{{"toString":0}}}}}}]"#
        );
        assert!(raw(&mut s, &result, context).is_none());
    }
    assert!(s.has_pending_repair());
    let (prompt, _, _) = continued(s.on_invalid_tool_input());
    assert!(has(&prompt, "attempt 1/5:"));
}

#[test]
fn pre_required_issues_and_flags_follow_source_specific_branch_order() {
    let cases = [
        ("write_file", false, false),
        ("edit_file", false, false),
        ("edit_file", true, false),
        ("edit_file", false, true),
    ];
    for (tool, truncated, repaired) in cases {
        let mut s = LargeFileRepair::new();
        let input = format!(
            r#"[{{"type":"error","toolName":"{tool}","error":{{"details":{{"issues":[null,[],1,{{"code":1}},{{"code":"required","path":false}}]}},"message":"ordinary"}}}}]"#
        );
        let expected = tool == "write_file" || truncated;
        assert_eq!(
            raw(
                &mut s,
                &input,
                LargeFileRepairToolContext {
                    output_truncated: truncated,
                    repaired_tool_calls: repaired,
                    finish_reason: Some(FinishReason::Length)
                }
            )
            .is_some(),
            expected
        );
    }
    // Original pre-draft branch has no permission-code exclusion. This prompt
    // classification grants no permission; post-draft exclusion is tested above.
    let mut s = LargeFileRepair::new();
    assert!(raw(&mut s,r#"[{"type":"error","toolName":"write_file","error":{"code":"permission_denied","message":"truncated"}}]"#,ORDINARY).is_some());
}

#[test]
fn any_post_truncation_flag_or_length_matches_without_required_issues() {
    for context in [
        LargeFileRepairToolContext {
            output_truncated: true,
            ..ORDINARY
        },
        LargeFileRepairToolContext {
            repaired_tool_calls: true,
            ..ORDINARY
        },
        LargeFileRepairToolContext {
            finish_reason: Some(FinishReason::Length),
            ..ORDINARY
        },
    ] {
        let mut s = LargeFileRepair::new();
        raw(
            &mut s,
            r#"[{"type":"success","toolName":"edit_file"}]"#,
            ORDINARY,
        );
        assert!(
            raw(
                &mut s,
                r#"[{"type":"error","toolName":"write_file","error":{"message":"ordinary"}}]"#,
                context
            )
            .is_some()
        );
    }
    let mut s = LargeFileRepair::new();
    assert!(
        raw(
            &mut s,
            r#"[{"type":"error","toolName":"bash","error":{"message":"truncated"}}]"#,
            ORDINARY
        )
        .is_none()
    );
}

#[test]
fn unicode_regex_fold_only_ascii_kelvin_and_long_s_without_utf16_loss() {
    for message in [
        "OUTPUT TOKEN limit",
        "required parameter `content` iſ miſſing",
        "prefix LARGE ARTIFACT suffix",
        "max_output",
        "too large",
    ] {
        let mut s = LargeFileRepair::new();
        let input = format!(
            r#"[{{"type":"error","toolName":"edit_file","error":{{"message":"{message}"}}}}]"#
        );
        assert!(raw(&mut s, &input, ORDINARY).is_some(), "{message}");
    }
    for message in ["large fıle", "large fİle", "truŉcated", "MAX-OUTPUT"] {
        let mut s = LargeFileRepair::new();
        let input = format!(
            r#"[{{"type":"error","toolName":"edit_file","error":{{"message":"{message}"}}}}]"#
        );
        assert!(raw(&mut s, &input, ORDINARY).is_none(), "{message}");
    }
    let mut s = LargeFileRepair::new();
    assert!(raw(&mut s,r#"[{"type":"error","toolName":"edit_file","error":{"message":"\ud800truncated\udfff"}}]"#,ORDINARY).is_some());
}

#[test]
fn recent_paths_are_exact_nonempty_record_strings_mru_dedup_five() {
    let mut s = LargeFileRepair::new();
    for input in [
        r#"[{"type":"success","toolName":"write_file","data":{"filePath":"a"}},{"type":"success","toolName":"edit_file","data":{"filePath":"b"}}]"#,
        r#"[{"type":"success","toolName":"write_file","data":{"filePath":"c"}},{"type":"success","toolName":"write_file","data":{"filePath":"d"}},{"type":"success","toolName":"write_file","data":{"filePath":"e"}},{"type":"success","toolName":"write_file","data":{"filePath":"f"}}]"#,
        r#"[{"type":"success","toolName":"edit_file","data":{"filePath":"c"}},{"type":"success","toolName":"write_file","data":{"filePath":""}},{"type":"success","toolName":"write_file","data":["ignored"]}]"#,
    ] {
        raw(&mut s, input, ORDINARY);
    }
    let (prompt, _, _) = continued(s.recover_from_repaired_truncation(&[call("write_file")]));
    assert!(has(
        &prompt,
        "Recent file path context for orientation only: c, f, e, d, b."
    ));
    let mut s = LargeFileRepair::new();
    raw(
        &mut s,
        r#"[{"type":"success","toolName":"write_file","data":{"filePath":"\ud800"}}]"#,
        ORDINARY,
    );
    let (prompt, _, _) = continued(s.recover_from_repaired_truncation(&[call("write_file")]));
    assert!(prompt.units().contains(&0xd800));
}

#[test]
fn raw_regex_tostring_and_property_errors_keep_source_short_circuit_order() {
    let mut s = LargeFileRepair::new();
    assert!(raw(&mut s,r#"[{"type":"error","toolName":"edit_file","error":{"message":[null,"truncated",false]}}]"#,ORDINARY).is_some());
    let mut s = LargeFileRepair::new();
    for input in [
        r#"{"type":"error","toolName":"edit_file","error":null}"#,
        r#"{"type":"error","toolName":"edit_file","error":{"message":{"toString":0}}}"#,
        r#"null"#,
    ] {
        assert!(
            s.analyze_boundary_results(&[snap(input)], ORDINARY)
                .is_err()
        );
    }
    assert_eq!(
        s.analyze_boundary_results(&[snap("null")], ORDINARY),
        Err(ProjectionError::SourceTypeError {
            message: "Cannot read properties of null (reading 'type')".into()
        })
    );
    assert_eq!(
        s.analyze_boundary_results(
            &[snap(r#"{"type":"error","toolName":"write_file"}"#)],
            ORDINARY
        ),
        Err(ProjectionError::SourceTypeError {
            message: "Cannot read properties of undefined (reading 'details')".into()
        })
    );
    assert_eq!(
        s.analyze_boundary_results(
            &[snap(
                r#"{"type":"error","toolName":"edit_file","error":{"message":{"toString":0}}}"#
            )],
            ORDINARY
        ),
        Err(ProjectionError::SourceTypeError {
            message: "Cannot convert object to primitive value".into()
        })
    );
    assert!(
        raw(
            &mut s,
            r#"[42,false,"unknown",{"type":"error","toolName":"edit_file","error":42}]"#,
            ORDINARY
        )
        .is_none()
    );
    // A required issue returns before regex conversion, so invalid toString is
    // never evaluated on this source path.
    assert!(raw(&mut s,r#"[{"type":"error","toolName":"write_file","error":{"details":{"issues":[{"code":"required"}]},"message":{"toString":0}}}]"#,ORDINARY).is_some());
}

#[test]
fn source_record_writes_happens_before_risk_and_keeps_partial_state_on_exception() {
    let mut s = LargeFileRepair::new();
    let inputs = [
        snap(r#"{"type":"success","toolName":"write_file","data":{"filePath":"saved"}}"#),
        snap("null"),
    ];
    assert!(s.analyze_boundary_results(&inputs, ORDINARY).is_err());
    let (prompt, purpose, _) = continued(s.recover_from_repaired_truncation(&[call("write_file")]));
    assert_eq!(
        purpose,
        JsString::from("large_file_repaired_truncation_after_write")
    );
    assert!(has(
        &prompt,
        "Recent file path context for orientation only: saved."
    ));
    // Successful write anywhere in the batch is recorded before classifying
    // the earlier error: this batch follows the post-draft rather than pre path.
    let mut s = LargeFileRepair::new();
    let (_, purpose, strip) = continued(raw(
        &mut s,
        r#"[{"type":"error","toolName":"write_file","error":{"message":"truncated"}},{"type":"success","toolName":"write_file"}]"#,
        ORDINARY,
    ));
    assert_eq!(purpose, JsString::from("large_file_post_draft_repair"));
    assert_eq!(strip, None);
}

fn typed(input: &str) -> ToolExecutionResult {
    let mut value = parse_json(input).unwrap();
    let o = match &mut value {
        JsValue::Object(o) => o,
        _ => panic!(),
    };
    for (key, val) in [
        ("toolCallId", JsValue::from("call")),
        ("content", JsValue::Array(vec![])),
        ("startedAt", JsValue::from("start")),
        ("completedAt", JsValue::from("end")),
    ] {
        o.insert(key.into(), val);
    }
    let disposition = if value
        .get("type")
        .unwrap()
        .as_string()
        .unwrap()
        .equals_str("success")
    {
        ToolResultDisposition::Success {
            data: value.get("data").cloned(),
        }
    } else {
        let error = value.get("error").unwrap().as_object().unwrap();
        ToolResultDisposition::Error {
            code: error.get("code").unwrap().as_string().unwrap().clone(),
            message: error.get("message").unwrap().as_string().unwrap().clone(),
            fields: error.clone(),
        }
    };
    ToolExecutionResult {
        tool_call_id: "call".into(),
        tool_name: value.get("toolName").unwrap().as_string().unwrap().clone(),
        disposition,
        content: vec![],
        supplemental_messages: None,
        metadata: None,
        started_at: "start".into(),
        completed_at: "end".into(),
        boundary: BoundarySnapshot::capture(&value),
    }
}

#[test]
fn typed_admission_matches_mandatory_boundary_and_rejects_contradiction_before_state_change() {
    let mut s = LargeFileRepair::new();
    let result = typed(
        r#"{"type":"error","toolName":"write_file","error":{"code":"invalid_tool_input","message":"ordinary","details":{"issues":[{"code":"required"}]}}}"#,
    );
    let before = result.boundary.value().clone();
    let (_, purpose, _) = continued(
        s.analyze_tool_results(std::slice::from_ref(&result), ORDINARY)
            .unwrap(),
    );
    assert_eq!(purpose, JsString::from("large_file_pre_draft_repair"));
    assert_eq!(result.boundary.value(), &before);
    let mut wrong = result;
    wrong.tool_name = "bash".into();
    let mut s = LargeFileRepair::new();
    assert!(matches!(
        s.analyze_tool_results(&[wrong], ORDINARY),
        Err(ProjectionError::InvalidBoundary { .. })
    ));
    assert!(!s.has_pending_repair());
    let mut success =
        typed(r#"{"type":"success","toolName":"write_file","data":{"filePath":"a"}}"#);
    success.disposition = ToolResultDisposition::Success { data: None };
    assert!(s.analyze_tool_results(&[success], ORDINARY).is_err());
}

#[test]
fn boundary_truncation_minimal_name_needs_no_id_or_input_and_preserves_echo() {
    let minimal = snap(r#"{"name":"write_file","extension":{"raw":"\ud800"}}"#);
    let original = minimal.clone();
    let mut state = LargeFileRepair::new();
    let (prompt, purpose, strip) = continued(
        state
            .recover_from_boundary_truncation(std::slice::from_ref(&minimal))
            .unwrap(),
    );
    assert_eq!(purpose, JsString::from("large_file_repaired_truncation"));
    assert_eq!(strip, Some(RepairStrip::Assistant));
    assert!(has(&prompt, "Recovery attempt 1/5."));
    assert_eq!(minimal.value(), original.value());
    assert!(minimal.value().get("id").is_none());
    assert!(minimal.value().get("input").is_none());
}

#[test]
fn boundary_truncation_property_set_membership_is_strict_not_string_coercion() {
    let mut state = LargeFileRepair::new();
    for value in [
        r#"{}"#,
        r#"42"#,
        r#"false"#,
        r#""write_file""#,
        r#"[]"#,
        r#"{"name":7}"#,
        r#"{"name":["write_file"]}"#,
        r#"{"name":"Write_file"}"#,
        r#"{"name":" write_file"}"#,
        r#"{"name":"\ud800write_file"}"#,
    ] {
        assert!(
            state
                .recover_from_boundary_truncation(&[snap(value)])
                .unwrap()
                .is_none(),
            "{value}"
        );
    }
    assert!(!state.has_pending_repair());
    assert!(
        state
            .recover_from_boundary_truncation(&[])
            .unwrap()
            .is_none()
    );
    assert!(
        state
            .recover_from_boundary_truncation(&[snap(r#"{"name":"edit_file"}"#)])
            .unwrap()
            .is_some()
    );
}

#[test]
fn boundary_nullish_first_throws_exact_source_typeerror_without_consuming_budget() {
    for (value, receiver) in [(JsValue::Null, "null"), (JsValue::Undefined, "undefined")] {
        let mut state = LargeFileRepair::new();
        let input = [
            BoundarySnapshot::capture(&value),
            snap(r#"{"name":"write_file"}"#),
        ];
        assert_eq!(
            state.recover_from_boundary_truncation(&input),
            Err(ProjectionError::SourceTypeError {
                message: format!("Cannot read properties of {receiver} (reading 'name')").into()
            })
        );
        assert!(!state.has_pending_repair());
        for _ in 0..10 {
            assert!(
                state
                    .recover_from_boundary_truncation(&[snap(r#"{"name":"write_file"}"#)])
                    .unwrap()
                    .is_some()
            );
        }
        assert!(
            state
                .recover_from_boundary_truncation(&[snap(r#"{"name":"write_file"}"#)])
                .unwrap()
                .is_none()
        );
    }
}

#[test]
fn boundary_some_short_circuits_after_first_write_before_later_nullish() {
    let mut state = LargeFileRepair::new();
    let input = [
        snap(r#"{"name":"lookup"}"#),
        snap(r#"{"name":"edit_file"}"#),
        snap("null"),
        BoundarySnapshot::capture(&JsValue::Undefined),
    ];
    for _ in 0..10 {
        assert!(
            state
                .recover_from_boundary_truncation(&input)
                .unwrap()
                .is_some()
        );
    }
    assert!(
        state
            .recover_from_boundary_truncation(&input)
            .unwrap()
            .is_none()
    );
}

#[test]
fn boundary_some_runs_before_ten_cap_and_still_throws_after_cap() {
    let mut state = LargeFileRepair::new();
    let input = [snap(r#"{"name":"write_file"}"#)];
    for _ in 0..10 {
        assert!(
            state
                .recover_from_boundary_truncation(&input)
                .unwrap()
                .is_some()
        );
    }
    assert_eq!(
        state.recover_from_boundary_truncation(&[snap("null")]),
        Err(ProjectionError::SourceTypeError {
            message: "Cannot read properties of null (reading 'name')".into()
        })
    );
    assert!(state.has_pending_repair());
    assert!(
        state
            .recover_from_boundary_truncation(&input)
            .unwrap()
            .is_none()
    );
}

#[test]
fn boundary_nonwrite_before_nullish_keeps_prior_pending_and_recovery_count() {
    let mut state = LargeFileRepair::new();
    let write = [snap(r#"{"name":"write_file"}"#)];
    state.recover_from_boundary_truncation(&write).unwrap();
    assert!(
        state
            .recover_from_boundary_truncation(&[snap(r#"{"name":"lookup"}"#), snap("null")])
            .is_err()
    );
    assert!(state.has_pending_repair());
    for _ in 0..9 {
        assert!(
            state
                .recover_from_boundary_truncation(&write)
                .unwrap()
                .is_some()
        );
    }
    assert!(
        state
            .recover_from_boundary_truncation(&write)
            .unwrap()
            .is_none()
    );
}

#[test]
fn boundary_multiple_writes_increment_only_one_truncation_and_not_pre_attempts() {
    let mut state = LargeFileRepair::new();
    let writes = [
        snap(r#"{"name":"write_file"}"#),
        snap(r#"{"name":"edit_file"}"#),
    ];
    for _ in 0..10 {
        let (prompt, _, _) = continued(state.recover_from_boundary_truncation(&writes).unwrap());
        assert!(has(&prompt, "Recovery attempt 1/5."));
    }
    let (prompt, _, _) = continued(state.on_invalid_tool_input());
    assert!(has(&prompt, "Recovery attempt 1/5."));
    assert!(
        state
            .recover_from_boundary_truncation(&writes)
            .unwrap()
            .is_none()
    );
}

#[test]
fn boundary_and_typed_truncation_share_the_same_independent_ten_budget() {
    let mut state = LargeFileRepair::new();
    for _ in 0..5 {
        assert!(
            state
                .recover_from_repaired_truncation(&[call("write_file")])
                .is_some()
        );
    }
    for _ in 0..5 {
        assert!(
            state
                .recover_from_boundary_truncation(&[snap(r#"{"name":"edit_file"}"#)])
                .unwrap()
                .is_some()
        );
    }
    assert!(
        state
            .recover_from_repaired_truncation(&[call("write_file")])
            .is_none()
    );
    assert!(
        state
            .recover_from_boundary_truncation(&[snap(r#"{"name":"write_file"}"#)])
            .unwrap()
            .is_none()
    );
}

#[test]
fn boundary_after_file_write_keeps_post_phase_zero_attempt_and_wtf16_path() {
    let mut state = LargeFileRepair::new();
    raw(
        &mut state,
        r#"[{"type":"success","toolName":"write_file","data":{"filePath":"\ud800"}}]"#,
        ORDINARY,
    );
    let (prompt, purpose, strip) = continued(
        state
            .recover_from_boundary_truncation(&[snap(r#"{"name":"write_file","input":null}"#)])
            .unwrap(),
    );
    assert_eq!(
        purpose,
        JsString::from("large_file_repaired_truncation_after_write")
    );
    assert_eq!(strip, Some(RepairStrip::Assistant));
    assert!(prompt.units().contains(&0xd800));
    assert!(has(&prompt, "Post-draft recovery attempt 0/5:"));
    let (prompt, _, strip) = continued(state.on_invalid_tool_input());
    assert!(has(&prompt, "Post-draft recovery attempt 1/5:"));
    assert_eq!(strip, None);
}

#[test]
fn boundary_missing_name_and_own_undefined_name_differ_from_undefined_entry() {
    let mut state = LargeFileRepair::new();
    let mut object = pilotdeck_protocol::JsObject::new();
    object.insert("name".into(), JsValue::Undefined);
    assert!(
        state
            .recover_from_boundary_truncation(&[
                snap("{}"),
                BoundarySnapshot::capture(&JsValue::Object(object))
            ])
            .unwrap()
            .is_none()
    );
    assert_eq!(
        state.recover_from_boundary_truncation(&[BoundarySnapshot::capture(&JsValue::Undefined)]),
        Err(ProjectionError::SourceTypeError {
            message: "Cannot read properties of undefined (reading 'name')".into()
        })
    );
    assert!(!state.has_pending_repair());
}

#[test]
fn boundary_name_only_recovery_matches_typed_decision_exactly() {
    let mut typed_state = LargeFileRepair::new();
    let mut boundary_state = LargeFileRepair::new();
    for name in ["lookup", "write_file", "edit_file", "write_file"] {
        assert_eq!(
            typed_state.recover_from_repaired_truncation(&[call(name)]),
            boundary_state
                .recover_from_boundary_truncation(&[snap(&format!(r#"{{"name":"{name}"}}"#))])
                .unwrap()
        );
    }
    assert_eq!(
        typed_state.on_no_tool_calls(),
        boundary_state.on_no_tool_calls()
    );
}
