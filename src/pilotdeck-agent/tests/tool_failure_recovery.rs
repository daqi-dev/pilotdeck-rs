use pilotdeck_agent::ports::PortError;
use pilotdeck_agent::tool_failures::{
    InvalidToolAction as Action, InvalidToolPlan, ToolFailureRecovery,
};
use pilotdeck_protocol::model::BoundarySnapshot;
use pilotdeck_protocol::{JsObject, JsString, JsValue, parse_json, stringify_json};

fn b(s: &str) -> BoundarySnapshot {
    BoundarySnapshot::capture(&parse_json(s).unwrap())
}
fn invalid(name: &str, message: &str) -> BoundarySnapshot {
    let mut error = JsObject::new();
    error.insert("code".into(), "invalid_tool_input".into());
    error.insert("message".into(), message.into());
    let mut result = JsObject::new();
    result.insert("type".into(), "error".into());
    result.insert("toolName".into(), name.into());
    result.insert("error".into(), JsValue::Object(error));
    result.insert(
        "content".into(),
        parse_json(r#"[{"type":"text","text":"bad"}]"#).unwrap(),
    );
    BoundarySnapshot::capture(&JsValue::Object(result))
}
fn field<'a>(v: &'a JsValue, k: &str) -> &'a JsValue {
    v.get(k).unwrap()
}
fn array(v: &JsValue) -> &[JsValue] {
    match v {
        JsValue::Array(v) => v,
        other => panic!("{other:?}"),
    }
}
fn text(v: &JsValue) -> &JsString {
    field(&array(field(v, "content"))[0], "text")
        .as_string()
        .unwrap()
}
fn keys(v: &JsValue) -> Vec<JsString> {
    v.as_object()
        .unwrap()
        .entries()
        .iter()
        .map(|(k, _)| k.clone())
        .collect()
}
fn source_error(e: PortError, expected: &str) {
    match e {
        PortError::SourceException { message, .. } => {
            assert!(message.equals_str(expected), "{message:?}")
        }
        other => panic!("{other:?}"),
    }
}
fn stop() -> InvalidToolPlan {
    InvalidToolPlan {
        action: Action::Stop {
            repeated_failures: 4,
        },
    }
}

#[test]
fn unique_then_same_batch_annotations_preserve_boundary_order_and_optional_keys() {
    let mut s = ToolFailureRecovery::new();
    let mut v = invalid("read", "bad").value().as_object().unwrap().clone();
    v.insert("metadata".into(), JsValue::Undefined);
    let original = BoundarySnapshot::capture(&JsValue::Object(v));
    assert_eq!(
        s.annotate_results(std::slice::from_ref(&original)).unwrap()[0].value(),
        original.value()
    );
    let out = s.annotate_results(std::slice::from_ref(&original)).unwrap();
    assert_eq!(keys(out[0].value()), keys(original.value()));
    assert!(
        text(out[0].value()).units().starts_with(
            &"bad\n\nRepeated failure: "
                .encode_utf16()
                .collect::<Vec<_>>()
        )
    );
    let metadata = field(out[0].value(), "metadata");
    assert_eq!(keys(metadata), vec![JsString::from("recovery")]);
    assert_eq!(
        keys(field(metadata, "recovery")),
        vec![JsString::from("avoidRetryReason"), "repeatedFailure".into()]
    );
    assert!(field(original.value(), "metadata").is_undefined());
}

#[test]
fn duplicates_within_batch_annotate_matching_errors_and_leave_success_unchanged() {
    let mut s = ToolFailureRecovery::new();
    let a = invalid("read", "one");
    let c = invalid("read", "different message");
    let other = invalid("write", "one");
    let success = b(r#"{"type":"success","content":null}"#);
    let out = s
        .annotate_results(&[a, success.clone(), other.clone(), c])
        .unwrap();
    assert!(
        text(out[0].value())
            .to_utf8()
            .unwrap()
            .contains("Repeated failure")
    );
    assert!(
        text(out[3].value())
            .to_utf8()
            .unwrap()
            .contains("Repeated failure")
    );
    assert_eq!(out[1].value(), success.value());
    assert_eq!(out[2].value(), other.value());
}

#[test]
fn failure_class_changes_repeated_fingerprint_and_arrays_are_not_recovery_records() {
    let mut s = ToolFailureRecovery::new();
    let a = b(
        r#"{"type":"error","toolName":"x","error":{"code":"c"},"content":[],"metadata":{"recovery":{"failureClass":"a"}}}"#,
    );
    let c = b(
        r#"{"type":"error","toolName":"x","error":{"code":"c"},"content":[],"metadata":{"recovery":{"failureClass":"b"}}}"#,
    );
    s.annotate_results(&[a]).unwrap();
    assert_eq!(
        s.annotate_results(std::slice::from_ref(&c)).unwrap()[0].value(),
        c.value()
    );
    let array = b(
        r#"{"type":"error","toolName":"x","error":{"code":"c"},"content":[],"metadata":{"recovery":[]}}"#,
    );
    s.annotate_results(std::slice::from_ref(&array)).unwrap();
    assert!(s.counters().0.unwrap().equals_str("x::c::unknown"));
}

#[test]
fn metadata_and_recovery_spreads_keep_numeric_keys_and_empty_reason() {
    let mut s = ToolFailureRecovery::new();
    let v = b(
        r#"{"type":"error","toolName":"x","error":{"code":"c"},"content":[],"metadata":{"tail":1,"recovery":{"z":null,"avoidRetryReason":"","repeatedFailure":false,"0":"zero"}}}"#,
    );
    let out = s.annotate_results(&[v.clone(), v]).unwrap();
    assert!(text(out[0].value()).equals_str("Repeated failure: \nChange at least one of the tool, parameters, path, scope, permission path, or explain the blocker in text."));
    let rec = field(field(out[0].value(), "metadata"), "recovery");
    assert_eq!(
        keys(rec),
        vec![
            JsString::from("0"),
            "z".into(),
            "avoidRetryReason".into(),
            "repeatedFailure".into()
        ]
    );
    assert_eq!(field(rec, "z"), &JsValue::Null);
    assert_eq!(field(rec, "repeatedFailure"), &JsValue::Bool(true));
}

#[test]
fn primitive_metadata_string_spread_preserves_utf16_units() {
    let mut s = ToolFailureRecovery::new();
    let v = b(
        r#"{"type":"error","toolName":"x","error":{"code":"c"},"content":[],"metadata":"\ud800A"}"#,
    );
    let out = s.annotate_results(&[v.clone(), v]).unwrap();
    let m = field(out[0].value(), "metadata");
    assert_eq!(
        keys(m),
        vec![JsString::from("0"), "1".into(), "recovery".into()]
    );
    assert_eq!(field(m, "0").as_string().unwrap().units(), &[0xd800]);
}

#[test]
fn first_falsy_content_discards_rest_and_nontext_is_preserved_after_prefix() {
    for first in ["null", "false", "0", "\"\""] {
        let v = b(&format!(
            r#"{{"type":"error","toolName":"x","error":{{"code":"c"}},"content":[{first},{{"type":"text","text":"discard"}}]}}"#
        ));
        let out = ToolFailureRecovery::new()
            .annotate_results(&[v.clone(), v])
            .unwrap();
        assert_eq!(array(field(out[0].value(), "content")).len(), 1);
    }
    let v = b(
        r#"{"type":"error","toolName":"x","error":{"code":"c"},"content":[7,{"type":"image","data":"keep"}]}"#,
    );
    let out = ToolFailureRecovery::new()
        .annotate_results(&[v.clone(), v])
        .unwrap();
    let c = array(field(out[0].value(), "content"));
    assert_eq!(c.len(), 3);
    assert_eq!(c[1], JsValue::Number(7.0));
}

#[test]
fn text_templates_preserve_null_missing_and_unpaired_surrogate() {
    for (raw, prefix) in [
        (r#"{"type":"text","text":null}"#, "null"),
        (r#"{"type":"text"}"#, "undefined"),
    ] {
        let v = b(&format!(
            r#"{{"type":"error","toolName":"x","error":{{"code":"c"}},"content":[{raw}]}}"#
        ));
        let out = ToolFailureRecovery::new()
            .annotate_results(&[v.clone(), v])
            .unwrap();
        assert!(text(out[0].value()).to_utf8().unwrap().starts_with(prefix));
    }
    let v = b(
        r#"{"type":"error","toolName":"x","error":{"code":"c"},"content":[{"type":"text","text":"\ud800"}]}"#,
    );
    let out = ToolFailureRecovery::new()
        .annotate_results(&[v.clone(), v])
        .unwrap();
    assert_eq!(text(out[0].value()).units()[0], 0xd800);
}

#[test]
fn string_content_iteration_keeps_pair_and_single_surrogate_as_items() {
    let v =
        b(r#"{"type":"error","toolName":"x","error":{"code":"c"},"content":"\ud83d\ude00\ud800"}"#);
    let out = ToolFailureRecovery::new()
        .annotate_results(&[v.clone(), v])
        .unwrap();
    let c = array(field(out[0].value(), "content"));
    assert_eq!(c.len(), 3);
    assert_eq!(c[1].as_string().unwrap().units(), &[0xd83d, 0xde00]);
    assert_eq!(c[2].as_string().unwrap().units(), &[0xd800]);
}

#[test]
fn annotation_failure_does_not_commit_new_fingerprint_or_mutate_inputs() {
    let mut s = ToolFailureRecovery::new();
    s.annotate_results(&[invalid("old", "m")]).unwrap();
    let previous = s.counters().0.unwrap().clone();
    let good = invalid("new", "m");
    let bad = b(
        r#"{"type":"error","toolName":"new","error":{"code":"invalid_tool_input"},"content":null}"#,
    );
    let before = good.clone();
    source_error(
        s.annotate_results(&[good.clone(), bad]).unwrap_err(),
        "content is not iterable",
    );
    assert_eq!(s.counters().0, Some(&previous));
    assert_eq!(good.value(), before.value());
}

#[test]
fn filter_reads_all_types_before_any_key_template_conversion() {
    let bad_name =
        b(r#"{"type":"error","toolName":{"toString":null},"error":{"code":"c"},"content":[]}"#);
    source_error(
        ToolFailureRecovery::new()
            .annotate_results(&[bad_name.clone(), b("null")])
            .unwrap_err(),
        "Cannot read properties of null (reading 'type')",
    );
    source_error(
        ToolFailureRecovery::new()
            .annotate_results(&[bad_name])
            .unwrap_err(),
        "Cannot convert object to primitive value",
    );
}

#[test]
fn utf16_sort_and_duplicate_keys_are_not_scalar_or_set_fingerprints() {
    let a = b(r#"{"type":"error","toolName":"\ud83d\ude00","error":{"code":"c"},"content":[]}"#);
    let c = b(r#"{"type":"error","toolName":"\ue000","error":{"code":"c"},"content":[]}"#);
    let mut s = ToolFailureRecovery::new();
    s.annotate_results(&[c, a.clone(), a]).unwrap();
    let mut expected = vec![0xd83d, 0xde00];
    expected.extend("::c::unknown\n".encode_utf16());
    expected.extend([0xd83d, 0xde00]);
    expected.extend("::c::unknown\n".encode_utf16());
    expected.push(0xe000);
    expected.extend("::c::unknown".encode_utf16());
    assert_eq!(s.counters().0.unwrap().units(), expected);
}

#[test]
fn templates_keep_array_join_nullish_and_js_numeric_strings() {
    let v = b(
        r#"{"type":"error","toolName":[1,null,[2,3]],"error":{"code":1e21},"content":[],"metadata":{"recovery":{"failureClass":false}}}"#,
    );
    let mut s = ToolFailureRecovery::new();
    s.annotate_results(&[v]).unwrap();
    assert!(s.counters().0.unwrap().equals_str("1,,2,3::1e+21::false"));
}

#[test]
fn all_invalid_nonmutating_query_short_circuits_and_preserves_throw_order() {
    assert!(!ToolFailureRecovery::all_invalid(&[]).unwrap());
    assert!(!ToolFailureRecovery::all_invalid(&[b(r#"{"type":"success"}"#), b("null")]).unwrap());
    source_error(
        ToolFailureRecovery::all_invalid(&[b(r#"{"type":"error","error":null}"#)]).unwrap_err(),
        "Cannot read properties of null (reading 'code')",
    );
    assert!(ToolFailureRecovery::all_invalid(&[invalid("a", "b")]).unwrap());
}

#[test]
fn same_invalid_group_gets_third_turn_grace_then_fourth_turn_stop() {
    let mut s = ToolFailureRecovery::new();
    let rs = [invalid("read", "same")];
    for count in 1..=2 {
        assert_eq!(
            s.select_invalid(&rs).unwrap().action,
            Action::FallThrough {
                reset_output_recovery: false
            }
        );
        assert_eq!(s.counters().2, count);
    }
    match s.select_invalid(&rs).unwrap().action {
        Action::Continue { prompt, purpose } => {
            assert!(purpose.equals_str("circuit_breaker_grace"));
            assert!(
                prompt
                    .to_utf8()
                    .unwrap()
                    .contains("try once more with corrected input.")
            );
        }
        other => panic!("{other:?}"),
    }
    assert!(s.counters().3);
    assert_eq!(
        s.select_invalid(&rs).unwrap().action,
        Action::Stop {
            repeated_failures: 4
        }
    );
}

#[test]
fn changed_invalid_message_renews_grace_even_after_prior_grace_consumed() {
    let mut s = ToolFailureRecovery::new();
    for _ in 0..3 {
        s.select_invalid(&[invalid("read", "one")]).unwrap();
    }
    s.select_invalid(&[invalid("read", "two")]).unwrap();
    assert_eq!(s.counters().2, 1);
    assert!(!s.counters().3);
    s.select_invalid(&[invalid("read", "two")]).unwrap();
    assert!(matches!(
        s.select_invalid(&[invalid("read", "two")]).unwrap().action,
        Action::Continue { .. }
    ));
}

#[test]
fn invalid_fingerprint_ignores_class_and_sorts_name_message_with_duplicates() {
    let mut s = ToolFailureRecovery::new();
    let a = b(
        r#"{"type":"error","toolName":"a","error":{"code":"invalid_tool_input","message":"m"},"metadata":{"recovery":{"failureClass":"first"}}}"#,
    );
    let c = b(
        r#"{"type":"error","toolName":"a","error":{"code":"invalid_tool_input","message":"m"},"metadata":{"recovery":{"failureClass":"second"}}}"#,
    );
    s.select_invalid(&[invalid("z", "b"), a]).unwrap();
    s.select_invalid(&[c, invalid("z", "b")]).unwrap();
    assert_eq!(s.counters().2, 2);
    assert!(s.counters().1.unwrap().equals_str("a::m\nz::b"));
    s.select_invalid(&[invalid("a", "m"), invalid("a", "m")])
        .unwrap();
    assert!(s.counters().1.unwrap().equals_str("a::m\na::m"));
    assert_eq!(s.counters().2, 1);
}

#[test]
fn mixed_known_errors_reset_invalid_fields_but_retain_annotation_fingerprint() {
    let mut s = ToolFailureRecovery::new();
    s.annotate_results(&[invalid("a", "m")]).unwrap();
    let fp = s.counters().0.unwrap().clone();
    for _ in 0..3 {
        s.select_invalid(&[invalid("a", "m")]).unwrap();
    }
    assert_eq!(
        s.select_invalid(&[b(r#"{"type":"error","error":{"code":"other"}}"#)])
            .unwrap()
            .action,
        Action::FallThrough {
            reset_output_recovery: true
        }
    );
    assert_eq!(s.counters(), (Some(&fp), None, 0, false));
    s.select_invalid(&[b(r#"{"type":"success"}"#)]).unwrap();
    assert_eq!(s.counters(), (None, None, 0, false));
}

#[test]
fn noninvalid_some_throw_retains_source_partial_resets_not_output_reset_success() {
    let mut s = ToolFailureRecovery::new();
    s.annotate_results(&[invalid("a", "m")]).unwrap();
    let fp = s.counters().0.unwrap().clone();
    for _ in 0..3 {
        s.select_invalid(&[invalid("a", "m")]).unwrap();
    }
    source_error(
        s.select_invalid(&[b(r#"{"type":"success"}"#), b("null")])
            .unwrap_err(),
        "Cannot read properties of null (reading 'type')",
    );
    assert_eq!(s.counters(), (Some(&fp), None, 0, false));
}

#[test]
fn invalid_template_throw_precedes_any_circuit_mutation() {
    let mut s = ToolFailureRecovery::new();
    s.select_invalid(&[invalid("a", "m")]).unwrap();
    let fp = s.counters().1.unwrap().clone();
    source_error(s.select_invalid(&[b(r#"{"type":"error","toolName":{"toString":null},"error":{"code":"invalid_tool_input","message":"m"}}"#)]).unwrap_err(), "Cannot convert object to primitive value");
    assert_eq!(s.counters(), (None, Some(&fp), 1, false));
}

#[test]
fn late_terminal_error_and_status_keep_source_order_and_own_undefined() {
    let p = stop();
    let e = p.terminal_error().unwrap();
    assert_eq!(
        keys(e.value()),
        vec![
            JsString::from("code"),
            "message".into(),
            "details".into(),
            "userHint".into()
        ]
    );
    assert!(field(e.value(), "details").is_undefined());
    assert!(
        !stringify_json(e.value())
            .unwrap()
            .unwrap()
            .contains("details")
    );
    let s = p.terminal_status(&e).unwrap();
    assert_eq!(
        keys(s.value()),
        vec![
            JsString::from("event"),
            "kind".into(),
            "text".into(),
            "detail".into()
        ]
    );
    assert_eq!(
        keys(field(s.value(), "detail")),
        vec![
            JsString::from("message"),
            "code".into(),
            "severity".into(),
            "visible".into(),
            "userHint".into(),
            "scope".into(),
            "source".into(),
            "repeatedFailures".into()
        ]
    );
    assert_eq!(
        field(field(s.value(), "detail"), "repeatedFailures"),
        &JsValue::Number(4.0)
    );
}

#[test]
fn late_status_truthiness_nullish_hint_and_type_errors_are_separate() {
    let p = stop();
    let s = p
        .terminal_status(&b(r#"{"message":0,"userHint":"","code":null}"#))
        .unwrap();
    assert!(
        field(s.value(), "text")
            .as_string()
            .unwrap()
            .equals_str("The agent repeatedly hit the same tool error, so this turn has stopped.")
    );
    assert_eq!(field(field(s.value(), "detail"), "code"), &JsValue::Null);
    assert!(
        field(field(s.value(), "detail"), "userHint")
            .as_string()
            .unwrap()
            .is_empty()
    );
    let s = p
        .terminal_status(&b(r#"{"message":[7],"userHint":false}"#))
        .unwrap();
    assert_eq!(
        field(s.value(), "text"),
        &JsValue::Array(vec![JsValue::Number(7.0)])
    );
    assert!(field(s.value(), "detail").get("code").is_none());
    assert_eq!(
        field(field(s.value(), "detail"), "userHint"),
        &JsValue::Bool(false)
    );
    source_error(
        p.terminal_status(&b("null")).unwrap_err(),
        "Cannot read properties of null (reading 'message')",
    );
}

#[test]
fn fallthrough_does_not_allow_terminal_construction() {
    let p = InvalidToolPlan {
        action: Action::FallThrough {
            reset_output_recovery: false,
        },
    };
    assert!(matches!(
        p.terminal_error(),
        Err(PortError::InvalidResponse { .. })
    ));
    assert!(matches!(
        p.terminal_status(&b("null")),
        Err(PortError::InvalidResponse { .. })
    ));
}
