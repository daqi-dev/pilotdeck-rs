//! Supplemental author controls derived from pinned Original sources.
//! Pure public proposals only: no host/turn/physical-durability proof.
use pilotdeck_agent::ProjectionError;
use pilotdeck_agent::ordinary_error_recovery::{
    JsonSelfCorrectionPlan, ModelInterruptedToolResultInput, create_model_interrupted_tool_result,
    plan_json_self_correction,
};
use pilotdeck_protocol::{JsObject, JsString, JsValue, model::BoundarySnapshot, parse_json};

fn capture(value: JsValue) -> BoundarySnapshot {
    BoundarySnapshot::capture(&value)
}
fn json(raw: &str) -> BoundarySnapshot {
    capture(parse_json(raw).unwrap())
}
fn obj<const N: usize>(fields: [(&str, JsValue); N]) -> JsValue {
    let mut result = JsObject::new();
    for (key, value) in fields {
        result.insert(key.into(), value);
    }
    JsValue::Object(result)
}
fn propose(call: &BoundarySnapshot) -> Result<BoundarySnapshot, ProjectionError> {
    create_model_interrupted_tool_result(ModelInterruptedToolResultInput {
        call,
        timestamp: &"2026-10-10T00:00:00.123Z".into(),
        cwd: &"/controlled-only".into(),
        permission_mode: &"ask".into(),
    })
}
fn text(result: &BoundarySnapshot) -> &JsString {
    let JsValue::Array(content) = result.value().get("content").unwrap() else {
        panic!("content array");
    };
    content[0].get("text").unwrap().as_string().unwrap()
}
fn source_error(result: Result<BoundarySnapshot, ProjectionError>, expected: &str) {
    assert!(
        matches!(result, Err(ProjectionError::SourceTypeError { message }) if message.equals_str(expected))
    );
}

#[test]
fn full_missing_result_preserves_source_advice_fields_order_and_timestamps() {
    let call =
        json(r#"{"id":"call-1","name":"write_file","input":{"path":"memory"},"extension":true}"#);
    let before = call.value().clone();
    let result = propose(&call).unwrap();
    let expected = json(
        r#"{
      "type":"error","toolCallId":"call-1","toolName":"write_file",
      "error":{"code":"tool_execution_failed","message":"Model error interrupted tool execution."},
      "content":[{"type":"text","text":"TOOL_ERROR[tool_execution_failed][write_file][environment_issue]\nSummary: Model error interrupted tool execution.\nEvidence:\n- Model error interrupted tool execution.\nOriginal error:\nModel error interrupted tool execution.\nNext actions:\n1. Inspect the evidence, change the approach, then retry only with corrected inputs. Current permission mode: ask."}],
      "metadata":{"recovery":{"summary":"Model error interrupted tool execution.","failureClass":"environment_issue","nextActions":["Inspect the evidence, change the approach, then retry only with corrected inputs. Current permission mode: ask."],"salientEvidence":["Model error interrupted tool execution."],"originalError":"Model error interrupted tool execution."}},
      "startedAt":"2026-10-10T00:00:00.123Z","completedAt":"2026-10-10T00:00:00.123Z"
    }"#,
    );
    assert_eq!(result.value(), expected.value());
    let keys: Vec<_> = result
        .value()
        .as_object()
        .unwrap()
        .entries()
        .iter()
        .map(|(key, _)| key.to_utf8().unwrap())
        .collect();
    assert_eq!(
        keys,
        [
            "type",
            "toolCallId",
            "toolName",
            "error",
            "content",
            "metadata",
            "startedAt",
            "completedAt"
        ]
    );
    let recovery = result
        .value()
        .get("metadata")
        .unwrap()
        .get("recovery")
        .unwrap();
    assert!(recovery.get("avoidRetryReason").is_none());
    assert_eq!(call.value(), &before);
}

#[test]
fn missing_and_null_fields_are_not_repaired_or_admission_rejected() {
    let missing = propose(&json("{}")).unwrap();
    assert!(matches!(
        missing.value().get("toolCallId"),
        Some(JsValue::Undefined)
    ));
    assert!(matches!(
        missing.value().get("toolName"),
        Some(JsValue::Undefined)
    ));
    assert!(
        text(&missing)
            .to_utf8()
            .unwrap()
            .starts_with("TOOL_ERROR[tool_execution_failed][undefined]")
    );
    let null = propose(&json(r#"{"id":null,"name":null}"#)).unwrap();
    assert!(matches!(
        null.value().get("toolCallId"),
        Some(JsValue::Null)
    ));
    assert!(
        text(&null)
            .to_utf8()
            .unwrap()
            .starts_with("TOOL_ERROR[tool_execution_failed][null]")
    );
    let primitive = propose(&capture(JsValue::Bool(false))).unwrap();
    assert!(matches!(
        primitive.value().get("toolCallId"),
        Some(JsValue::Undefined)
    ));
}

#[test]
fn wtf16_name_timestamp_and_permission_are_lossless_without_truncation() {
    let name = JsString::from_utf16(vec![0xd800, 0x61, 0xdcff]);
    let timestamp = JsString::from_utf16(vec![0xdc00]);
    let permission = JsString::from_utf16(vec![0xdfff]);
    let call = capture(obj([
        ("id", JsValue::Number(-0.0)),
        ("name", JsValue::String(name.clone())),
    ]));
    let result = create_model_interrupted_tool_result(ModelInterruptedToolResultInput {
        call: &call,
        timestamp: &timestamp,
        cwd: &"unused".into(),
        permission_mode: &permission,
    })
    .unwrap();
    assert_eq!(
        result.value().get("toolName").unwrap().as_string(),
        Some(&name)
    );
    assert_eq!(
        result.value().get("startedAt").unwrap().as_string(),
        Some(&timestamp)
    );
    assert_eq!(
        result.value().get("completedAt").unwrap().as_string(),
        Some(&timestamp)
    );
    assert_eq!(
        result
            .value()
            .get("toolCallId")
            .unwrap()
            .as_number()
            .unwrap()
            .to_bits(),
        (-0.0_f64).to_bits()
    );
    assert!(
        text(&result)
            .units()
            .windows(name.units().len())
            .any(|window| window == name.units())
    );
    assert!(text(&result).units().ends_with(&[0xdfff, 0x2e]));
}

#[test]
fn raw_name_templates_follow_join_and_number_semantics() {
    for (name, expected) in [
        (
            JsValue::Array(vec![
                JsValue::Null,
                JsValue::String("a".into()),
                JsValue::Array(vec!["b".into(), JsValue::Undefined]),
            ]),
            ",a,b,",
        ),
        (obj([]), "[object Object]"),
        (JsValue::Number(-0.0), "0"),
        (JsValue::Number(f64::NAN), "NaN"),
        (JsValue::Number(f64::INFINITY), "Infinity"),
    ] {
        let result = propose(&capture(obj([("name", name)]))).unwrap();
        assert!(
            text(&result)
                .to_utf8()
                .unwrap()
                .starts_with(&format!("TOOL_ERROR[tool_execution_failed][{expected}]"))
        );
    }
}

#[test]
fn original_name_property_and_primitive_conversion_errors_stay_source_errors() {
    source_error(
        propose(&capture(JsValue::Null)),
        "Cannot read properties of null (reading 'name')",
    );
    source_error(
        propose(&capture(JsValue::Undefined)),
        "Cannot read properties of undefined (reading 'name')",
    );
    source_error(
        propose(&json(r#"{"id":"valid","name":{"toString":null}}"#)),
        "Cannot convert object to primitive value",
    );
    // Nested array ToString reaches the same failing object; no generic advice
    // fallback or fabricated result can hide that Original exception.
    source_error(
        propose(&json(r#"{"name":["ok",{"toString":false}]}"#)),
        "Cannot convert object to primitive value",
    );
}

#[test]
fn duplicate_and_open_ids_retain_each_raw_call_without_minting_authority() {
    let calls = [
        json(r#"{"id":"same","name":"bash"}"#),
        json(r#"{"id":"same","name":"web_fetch"}"#),
        json(r#"{"id":{"raw":1},"name":7}"#),
    ];
    let results: Vec<_> = calls.iter().map(|call| propose(call).unwrap()).collect();
    for (call, result) in calls.iter().zip(&results) {
        assert_eq!(result.value().get("toolCallId"), call.value().get("id"));
        assert_eq!(result.value().get("toolName"), call.value().get("name"));
    }
    assert!(
        text(&results[1])
            .to_utf8()
            .unwrap()
            .contains("[web_fetch][environment_issue]")
    );
}

#[test]
fn json_correction_is_strict_enabled_and_exactly_three_with_exact_prompt() {
    let error = json(r#"{"code":"invalid_tool_arguments","message":"raw","extra":null}"#);
    let before = error.value().clone();
    for used in 0..3 {
        let JsonSelfCorrectionPlan::Continue {
            attempts_used,
            prompt,
            purpose,
        } = plan_json_self_correction(&error, true, used).unwrap()
        else {
            panic!("must continue");
        };
        assert_eq!(attempts_used, used + 1);
        assert!(purpose.equals_str("json_self_correct"));
        assert!(prompt.equals_str("Your previous tool call contained invalid JSON in the arguments and could not be parsed. Please retry with valid JSON. Common issues: missing quotes around keys/values, trailing commas, unescaped special characters in strings."));
    }
    for used in [3, 4, u32::MAX] {
        assert!(
            matches!(plan_json_self_correction(&error, true, used).unwrap(), JsonSelfCorrectionPlan::Decline { attempts_used } if attempts_used == used)
        );
    }
    assert_eq!(error.value(), &before);
}

#[test]
fn other_codes_and_coercible_arrays_do_not_spend_json_retry_budget() {
    for error in [
        json("{}"),
        json(r#"{"code":null}"#),
        json(r#"{"code":["invalid_tool_arguments"]}"#),
        json(r#"{"code":"INVALID_TOOL_ARGUMENTS"}"#),
        json(r#"{"code":"network_error"}"#),
        capture(JsValue::Number(1.0)),
    ] {
        assert!(matches!(
            plan_json_self_correction(&error, true, 2).unwrap(),
            JsonSelfCorrectionPlan::Decline { attempts_used: 2 }
        ));
    }
    assert!(matches!(
        plan_json_self_correction(&json(r#"{"code":"invalid_tool_arguments"}"#), false, 1).unwrap(),
        JsonSelfCorrectionPlan::Decline { attempts_used: 1 }
    ));
}

#[test]
fn disabled_skips_code_property_but_enabled_exhausted_still_reads_it() {
    let null = capture(JsValue::Null);
    assert!(matches!(
        plan_json_self_correction(&null, false, 0).unwrap(),
        JsonSelfCorrectionPlan::Decline { attempts_used: 0 }
    ));
    for used in [0, 3] {
        assert!(
            matches!(plan_json_self_correction(&null, true, used), Err(ProjectionError::SourceTypeError { message }) if message.equals_str("Cannot read properties of null (reading 'code')"))
        );
    }
}
