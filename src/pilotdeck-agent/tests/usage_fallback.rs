use pilotdeck_agent::ports::PortError;
use pilotdeck_agent::usage_fallback::{
    BudgetUsageContinuation, BudgetUsageInput, BudgetUsageSelection, OriginalBudgetUsageFallback,
};
use pilotdeck_protocol::model::BoundarySnapshot;
use pilotdeck_protocol::{JsObject, JsString, JsValue, parse_json};

fn obj(entries: Vec<(&str, JsValue)>) -> JsValue {
    let mut o = JsObject::new();
    for (k, v) in entries {
        o.insert(k.into(), v);
    }
    JsValue::Object(o)
}
fn block(kind: &str, text: JsValue) -> JsValue {
    obj(vec![("type", kind.into()), ("text", text)])
}
fn tool(input: JsValue) -> JsValue {
    obj(vec![("type", "tool_call".into()), ("input", input)])
}
fn snapshot(value: JsValue) -> BoundarySnapshot {
    BoundarySnapshot::capture(&value)
}
fn message(blocks: Vec<JsValue>) -> BoundarySnapshot {
    snapshot(obj(vec![("content", JsValue::Array(blocks))]))
}
fn select(
    usage: Option<&BoundarySnapshot>,
    input: Option<f64>,
    message: &BoundarySnapshot,
) -> Result<BudgetUsageSelection, PortError> {
    OriginalBudgetUsageFallback.select(BudgetUsageInput {
        usage,
        estimated_input_tokens: input,
        assistant_message: message,
    })
}
fn continuation(
    usage: Option<&BoundarySnapshot>,
    input: Option<f64>,
    message: &BoundarySnapshot,
) -> BudgetUsageContinuation {
    match select(usage, input, message).unwrap() {
        BudgetUsageSelection::NeedsOutputEstimate(c) => c,
        _ => panic!("Source fallback required"),
    }
}
fn text(blocks: Vec<JsValue>) -> JsString {
    continuation(None, None, &message(blocks))
        .response_text()
        .clone()
}
fn source_error(value: &BoundarySnapshot) -> JsString {
    match select(None, None, value).unwrap_err() {
        PortError::SourceException { message, .. } => message,
        _ => panic!("Source exception required"),
    }
}
#[test]
fn finite_unknown_provider_extension_shortcircuits_malformed_message() {
    let usage = snapshot(obj(vec![
        ("extension", JsValue::Number(-5.25)),
        ("unknown", JsValue::Undefined),
    ]));
    match select(Some(&usage), None, &snapshot(JsValue::Null)).unwrap() {
        BudgetUsageSelection::Original(Some(value)) => {
            assert_eq!(
                value.value().get("extension").unwrap().as_number(),
                Some(-5.25)
            );
            assert!(value.value().get("unknown").unwrap().is_undefined());
        }
        _ => panic!("Finite provider wins"),
    }
}
#[test]
fn provider_zero_negative_and_fractional_are_all_finite() {
    for value in [0.0, -0.0, -100.0, 0.25] {
        let usage = snapshot(obj(vec![("arbitrary", JsValue::Number(value))]));
        assert!(matches!(
            select(Some(&usage), Some(20.0), &snapshot(JsValue::Undefined)).unwrap(),
            BudgetUsageSelection::Original(_)
        ));
    }
}
#[test]
fn finite_array_index_is_own_usage_value() {
    let usage = snapshot(JsValue::Array(vec![JsValue::Null, JsValue::Number(1.0)]));
    assert!(matches!(
        select(Some(&usage), None, &snapshot(JsValue::Null)).unwrap(),
        BudgetUsageSelection::Original(_)
    ));
}
#[test]
fn nonfinite_or_nested_numbers_do_not_shortcut() {
    for value in [
        JsValue::Number(f64::NAN),
        JsValue::Number(f64::INFINITY),
        JsValue::Number(f64::NEG_INFINITY),
        obj(vec![("nested", JsValue::Number(3.0))]),
        JsValue::from("1"),
        JsValue::Bool(true),
    ] {
        let usage = snapshot(obj(vec![("field", value)]));
        assert!(matches!(
            select(Some(&usage), None, &message(vec![])).unwrap(),
            BudgetUsageSelection::NeedsOutputEstimate(_)
        ));
    }
}
#[test]
fn primitive_usage_is_boxed_without_numeric_own_values() {
    for value in [
        JsValue::Number(4.0),
        JsValue::Bool(true),
        JsValue::from("123"),
        JsValue::Null,
        JsValue::Undefined,
    ] {
        let usage = snapshot(value);
        assert!(matches!(
            select(Some(&usage), None, &message(vec![])).unwrap(),
            BudgetUsageSelection::NeedsOutputEstimate(_)
        ));
    }
}
#[test]
fn text_thinking_and_whitespace_are_kept_without_trim() {
    assert_eq!(
        text(vec![
            block("text", "  x  ".into()),
            block("thinking", " hidden ".into()),
            block("image", JsValue::Null)
        ]),
        JsString::from("  x  \n hidden \n")
    );
}
#[test]
fn nontext_blocks_still_contribute_newline_slots() {
    assert_eq!(
        text(vec![
            block("image", "unused".into()),
            block("unknown", "ignored".into()),
            block("text", "x".into())
        ]),
        JsString::from("\n\nx")
    );
}
#[test]
fn special_token_spellings_remain_ordinary_response_text() {
    assert_eq!(
        text(vec![block("text", "<|endoftext|> <|fim_prefix|>".into())]),
        JsString::from("<|endoftext|> <|fim_prefix|>")
    );
}
#[test]
fn null_undefined_and_absent_text_join_empty_entries() {
    assert_eq!(
        text(vec![
            block("text", JsValue::Null),
            block("thinking", JsValue::Undefined),
            obj(vec![("type", "text".into())])
        ]),
        JsString::from("\n\n")
    );
}
#[test]
fn text_coercion_has_js_numeric_boolean_and_array_spelling() {
    assert_eq!(
        text(vec![
            block("text", JsValue::Number(-0.0)),
            block("thinking", JsValue::Bool(false)),
            block(
                "text",
                JsValue::Array(vec![
                    JsValue::Null,
                    JsValue::Array(vec![JsValue::Number(1.0), JsValue::Undefined])
                ])
            )
        ]),
        JsString::from("0\nfalse\n,1,")
    );
}
#[test]
fn text_nonfinite_numeric_spelling_differs_from_tool_json() {
    assert_eq!(
        text(vec![
            block("text", JsValue::Number(f64::NAN)),
            block("text", JsValue::Number(f64::INFINITY)),
            block("text", JsValue::Number(f64::NEG_INFINITY)),
            tool(JsValue::Number(f64::NAN))
        ]),
        JsString::from("NaN\nInfinity\n-Infinity\nnull")
    );
}
#[test]
fn tool_string_is_used_verbatim_while_nullish_becomes_empty_object() {
    assert_eq!(
        text(vec![
            tool("raw \"not json\"".into()),
            tool(JsValue::Null),
            tool(JsValue::Undefined),
            obj(vec![("type", "tool_call".into())])
        ]),
        JsString::from("raw \"not json\"\n{}\n{}\n{}")
    );
}
#[test]
fn tool_other_falsy_values_are_not_nullish_defaults() {
    assert_eq!(
        text(vec![
            tool(JsValue::Bool(false)),
            tool(JsValue::Number(0.0)),
            tool("".into())
        ]),
        JsString::from("false\n0\n")
    );
}
#[test]
fn tool_json_preserves_js_key_order_and_undefined_rules() {
    let value = obj(vec![
        ("z", JsValue::Undefined),
        ("2", "two".into()),
        ("1", "one".into()),
        (
            "a",
            JsValue::Array(vec![JsValue::Undefined, JsValue::Number(f64::INFINITY)]),
        ),
    ]);
    assert_eq!(
        text(vec![tool(value)]),
        JsString::from("{\"1\":\"one\",\"2\":\"two\",\"a\":[null,null]}")
    );
}
#[test]
fn utf16_text_keeps_unpaired_surrogate_tool_json_escapes_it() {
    let s = JsString::from_utf16(vec![0xd800, 0x78]);
    let mut expected = vec![0xd800, 0x78, 10];
    expected.extend("{\"s\":\"\\ud800x\"}".encode_utf16());
    assert_eq!(
        text(vec![
            block("text", JsValue::String(s.clone())),
            tool(obj(vec![("s", JsValue::String(s))]))
        ]),
        JsString::from_utf16(expected)
    );
    assert_eq!(
        text(vec![
            block(
                "text",
                JsValue::String(JsString::from_utf16(vec![0xd800, 0x78]))
            ),
            tool(JsValue::String(JsString::from_utf16(vec![0xd800, 0x78])))
        ])
        .units(),
        &[0xd800, 0x78, 10, 0xd800, 0x78]
    );
    assert_eq!(
        text(vec![tool(JsValue::Array(vec![JsValue::String(
            JsString::from_utf16(vec![0xd800, 0x78])
        )]))]),
        JsString::from("[\"\\ud800x\"]")
    );
}
#[test]
fn all_map_entries_precede_earlier_text_join_failure() {
    let bad = obj(vec![("toString", JsValue::Null)]);
    let msg = message(vec![block("text", bad), JsValue::Null]);
    assert_eq!(
        source_error(&msg),
        JsString::from("Cannot read properties of null (reading 'type')")
    );
}
#[test]
fn join_noncallable_to_string_throws_after_successful_map() {
    assert_eq!(
        source_error(&message(vec![
            block("text", obj(vec![("toString", JsValue::Null)])),
            block("image", JsValue::Null)
        ])),
        JsString::from("Cannot convert object to primitive value")
    );
}
#[test]
fn ordinary_object_text_uses_default_string_conversion() {
    assert_eq!(
        text(vec![block("text", obj(vec![("valueOf", JsValue::Null)]))]),
        JsString::from("[object Object]")
    );
}
#[test]
fn malformed_content_preserves_source_map_errors() {
    assert_eq!(
        source_error(&snapshot(JsValue::Null)),
        JsString::from("Cannot read properties of null (reading 'content')")
    );
    assert_eq!(
        source_error(&snapshot(obj(vec![]))),
        JsString::from("Cannot read properties of undefined (reading 'map')")
    );
    assert_eq!(
        source_error(&snapshot(obj(vec![("content", JsValue::Null)]))),
        JsString::from("Cannot read properties of null (reading 'map')")
    );
    assert_eq!(
        source_error(&snapshot(obj(vec![("content", "notarray".into())]))),
        JsString::from("assistantMessage.content.map is not a function")
    );
}
#[test]
fn primitive_unknown_blocks_map_to_empty_instead_of_getting_rejected() {
    assert_eq!(
        text(vec![
            JsValue::Number(2.0),
            JsValue::from("x"),
            JsValue::Bool(false)
        ]),
        JsString::from("\n\n")
    );
}
#[test]
fn zero_output_and_absent_input_return_original_usage_verbatim() {
    let usage = snapshot(obj(vec![
        ("unknown", JsValue::Undefined),
        ("code", JsValue::Null),
    ]));
    let result = continuation(Some(&usage), None, &message(vec![]))
        .resolve(0)
        .unwrap();
    assert_eq!(result.value(), usage.value());
    assert!(
        continuation(None, None, &message(vec![]))
            .resolve(0)
            .is_none()
    );
}
#[test]
fn defined_zero_input_is_retained_even_with_zero_output() {
    let result = continuation(None, Some(0.0), &message(vec![]))
        .resolve(0)
        .unwrap();
    assert_eq!(
        result.value(),
        &parse_json("{\"inputTokens\":0,\"totalTokens\":0}").unwrap()
    );
}
#[test]
fn positive_output_counts_are_injected_not_inferred_from_text() {
    let result = continuation(None, None, &message(vec![]))
        .resolve(7)
        .unwrap();
    assert_eq!(
        result.value(),
        &parse_json("{\"outputTokens\":7,\"totalTokens\":7}").unwrap()
    );
}
#[test]
fn input_plus_positive_output_field_order_and_total_match_source() {
    let result = continuation(None, Some(1.25), &message(vec![]))
        .resolve(8)
        .unwrap();
    assert_eq!(
        result.value(),
        &parse_json("{\"inputTokens\":1.25,\"outputTokens\":8,\"totalTokens\":9.25}").unwrap()
    );
}
#[test]
fn lax_nonfinite_estimate_is_preserved_without_invented_validation() {
    let result = continuation(None, Some(f64::NAN), &message(vec![]))
        .resolve(0)
        .unwrap();
    assert!(
        result
            .value()
            .get("inputTokens")
            .unwrap()
            .as_number()
            .unwrap()
            .is_nan()
    );
    assert!(
        result
            .value()
            .get("totalTokens")
            .unwrap()
            .as_number()
            .unwrap()
            .is_nan()
    );
}
#[test]
fn deferred_capture_detaches_original_usage_and_message() {
    let mut usage = obj(vec![("ext", JsValue::Null)]);
    let original = snapshot(usage.clone());
    let c = continuation(
        Some(&original),
        None,
        &message(vec![block("text", "x".into())]),
    );
    if let JsValue::Object(o) = &mut usage {
        o.insert("ext".into(), "changed".into());
    }
    assert_eq!(c.response_text(), &JsString::from("x"));
    assert!(matches!(
        c.resolve(0).unwrap().value().get("ext"),
        Some(JsValue::Null)
    ));
}
