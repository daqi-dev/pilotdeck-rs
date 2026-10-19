//! Source-derived pure budget terminal checks. No charge or owner-effect gate.
use budget_terminal::{BudgetTerminalInput, BudgetTerminalPlan};
use driver::{BudgetLedger, BudgetStop};
use pilotdeck_agent::{
    MessageProjection, ProjectedMessage, ProjectionGap, budget_terminal, driver, ports, turn,
};
use pilotdeck_protocol::model::{
    BoundarySnapshot, ContentBlock, ContentBody, Message, Role, TextBlock,
};
use pilotdeck_protocol::{JsObject, JsString, JsValue};
use ports::{PortError, PortResult, Route};
use turn::TurnStopReason;
#[path = "../src/budget_terminal_policy.rs"]
mod budget_terminal_policy;

fn obj(fields: impl IntoIterator<Item = (&'static str, JsValue)>) -> JsValue {
    let mut object = JsObject::new();
    for (key, value) in fields {
        object.insert(key.into(), value);
    }
    JsValue::Object(object)
}
fn block(kind: &str, text: JsValue) -> JsValue {
    obj([("type", kind.into()), ("text", text)])
}
fn raw_message(content: JsValue) -> JsValue {
    obj([
        ("role", "assistant".into()),
        ("content", content),
        ("extension", JsValue::Undefined),
    ])
}
fn project(value: JsValue) -> ProjectedMessage {
    ProjectedMessage {
        boundary: BoundarySnapshot::capture(&value),
        normalized: MessageProjection::BoundaryOnly(vec![ProjectionGap::OtherBoundaryShape]),
    }
}
fn ledger() -> BudgetLedger {
    BudgetLedger {
        turn_limit_usd: Some(0.0078125),
        task_limit_usd: None,
        initial_task_spent_usd: 0.1,
        task_spent_usd: 0.125,
        turn_spent_usd: 0.0078125,
    }
}
fn evaluate(
    l: &BudgetLedger,
    stop: BudgetStop,
    m: &ProjectedMessage,
    count: usize,
    cost: Option<f64>,
) -> PortResult<BudgetTerminalPlan> {
    let route = Route {
        provider: JsString::from_utf16(vec![0xd800]),
        model: "model".into(),
    };
    budget_terminal_policy::evaluate(BudgetTerminalInput {
        ledger: l,
        stop,
        last_invocation_cost_usd: cost,
        route: &route,
        assistant_message: m,
        unexecuted_tool_call_count: count,
    })
}
fn safe(raw: JsValue) -> PortResult<Option<ProjectedMessage>> {
    Ok(evaluate(&ledger(), BudgetStop::Turn, &project(raw), 0, None)?.safe_message)
}
fn fixed(value: f64, expected: &str) {
    assert_eq!(pilotdeck_protocol::number::to_fixed_6(value), expected);
}

#[test]
fn binary_half_ties_round_up_not_even() {
    fixed(0.0078125, "0.007813");
    fixed(-0.0078125, "-0.007813");
}
#[test]
fn adjacent_half_neighbors_are_not_float_multiply_rounded() {
    fixed(f64::from_bits(0.0078125_f64.to_bits() - 1), "0.007812");
    fixed(f64::from_bits(0.0078125_f64.to_bits() + 1), "0.007813");
}
#[test]
fn signed_zero_and_negative_underflow() {
    fixed(-0.0, "0.000000");
    fixed(0.0, "0.000000");
    fixed(-f64::from_bits(1), "-0.000000");
    fixed(f64::from_bits(1), "0.000000");
}
#[test]
fn exact_large_binary_integer_not_shortest_decimal() {
    fixed(1_000_000_000_000_000_128_f64, "1000000000000000128.000000");
    fixed(
        f64::from_bits(1e21_f64.to_bits() - 1),
        "999999999999999868928.000000",
    );
}
#[test]
fn exponent_threshold_and_maximum_finite() {
    fixed(1e21, "1e+21");
    fixed(-1e21, "-1e+21");
    fixed(f64::MAX, "1.7976931348623157e+308");
}
#[test]
fn nonfinite_to_fixed_is_not_json_null() {
    fixed(f64::NAN, "NaN");
    fixed(f64::INFINITY, "Infinity");
    fixed(f64::NEG_INFINITY, "-Infinity");
}
#[test]
fn source_budget_error_detail_undefined_and_route_units() {
    let m = project(raw_message(JsValue::Array(vec![block(
        "text",
        "hello".into(),
    )])));
    let l = ledger();
    let plan = evaluate(&l, BudgetStop::Turn, &m, 0, None).unwrap();
    assert_eq!(
        plan.error.value().get("code"),
        Some(&JsValue::from("agent_max_budget_reached"))
    );
    assert_eq!(
        plan.error.value().get("message"),
        Some(&JsValue::from(
            "Reached Gateway-owned maxBudgetUsd ($0.007813) after spending $0.125000."
        ))
    );
    assert_eq!(
        plan.error.value().get("userHint"),
        Some(&JsValue::from(
            "Increase maxBudgetUsd or start a new turn with a larger budget."
        ))
    );
    assert_eq!(
        plan.status_detail.value(),
        plan.error.value().get("details").unwrap()
    );
    assert!(matches!(
        plan.status_detail.value().get("lastInvocationCostUsd"),
        Some(JsValue::Undefined)
    ));
    assert!(plan.status_detail.value().get("taskBudgetUsd").is_none());
    assert_eq!(
        plan.status_detail
            .value()
            .get("provider")
            .unwrap()
            .as_string()
            .unwrap()
            .units(),
        &[0xd800]
    );
    assert!(plan.status_event.equals_str("max_budget_reached"));
    assert_eq!(plan.stop_reason, TurnStopReason::MaxBudget);
    assert_eq!(l.task_spent_usd, 0.125);
    assert_eq!(l.turn_spent_usd, 0.0078125);
}
#[test]
fn task_branch_uses_total_spent_and_exact_hint() {
    let mut l = ledger();
    l.turn_limit_usd = Some(1.0);
    l.task_limit_usd = Some(0.1);
    let plan = evaluate(
        &l,
        BudgetStop::Task,
        &project(raw_message(JsValue::Array(vec![]))),
        0,
        Some(0.0),
    )
    .unwrap();
    assert_eq!(
        plan.error.value().get("code"),
        Some(&JsValue::from("agent_task_budget_reached"))
    );
    assert_eq!(
        plan.error.value().get("message"),
        Some(&JsValue::from(
            "Reached Gateway-owned taskBudget.total ($0.100000) after spending $0.125000."
        ))
    );
    assert_eq!(
        plan.error.value().get("userHint"),
        Some(&JsValue::from(
            "Increase taskBudget.total or start a new SDK session with a larger budget."
        ))
    );
    assert!(plan.status_event.equals_str("task_budget_reached"));
    assert_eq!(plan.stop_reason, TurnStopReason::TaskBudget);
    assert_eq!(
        plan.status_detail.value().get("lastInvocationCostUsd"),
        Some(&JsValue::Number(0.0))
    );
}
#[test]
fn both_reached_turn_has_strict_priority() {
    let mut l = ledger();
    l.task_limit_usd = Some(0.1);
    let m = project(raw_message(JsValue::Array(vec![])));
    assert_eq!(
        evaluate(&l, BudgetStop::Turn, &m, 0, None)
            .unwrap()
            .stop_reason,
        TurnStopReason::MaxBudget
    );
    assert!(matches!(
        evaluate(&l, BudgetStop::Task, &m, 0, None),
        Err(PortError::InvalidResponse { .. })
    ));
}
#[test]
fn absent_or_unreached_selected_limit_is_caller_invariant() {
    let mut l = ledger();
    l.turn_limit_usd = None;
    let m = project(JsValue::Null);
    assert!(matches!(
        evaluate(&l, BudgetStop::Turn, &m, 0, None),
        Err(PortError::InvalidResponse { .. })
    ));
    l.turn_limit_usd = Some(1.0);
    assert!(matches!(
        evaluate(&l, BudgetStop::Turn, &m, 0, None),
        Err(PortError::InvalidResponse { .. })
    ));
}
#[test]
fn unexecuted_tools_suppress_before_malformed_message_read() {
    let plan = evaluate(
        &ledger(),
        BudgetStop::Turn,
        &project(JsValue::Null),
        1,
        None,
    )
    .unwrap();
    assert!(plan.safe_message.is_none());
}
#[test]
fn remove_only_thinking_preserve_media_unknown_slots() {
    let media = obj([
        ("type", "image".into()),
        ("data", JsValue::Undefined),
        ("url", JsValue::Null),
        ("x", JsValue::Array(vec![JsValue::Undefined])),
    ]);
    let raw = raw_message(JsValue::Array(vec![
        block("thinking", "secret".into()),
        block("text", "hello".into()),
        media.clone(),
        JsValue::Number(7.0),
    ]));
    let out = safe(raw.clone()).unwrap().unwrap();
    assert_eq!(
        out.boundary.value().get("content"),
        Some(&JsValue::Array(vec![
            block("text", "hello".into()),
            media,
            JsValue::Number(7.0)
        ]))
    );
    assert!(matches!(
        out.boundary.value().get("extension"),
        Some(JsValue::Undefined)
    ));
    assert!(matches!(raw.get("content"),Some(JsValue::Array(blocks)) if blocks.len()==4));
}
#[test]
fn thinking_with_invalid_text_is_never_coerced() {
    assert!(
        safe(raw_message(JsValue::Array(vec![
            block("thinking", obj([("toString", JsValue::Null)])),
            block("text", "visible".into())
        ])))
        .unwrap()
        .is_some()
    );
}
#[test]
fn all_ecmascript_whitespace_is_empty_but_nel_is_not() {
    let spaces = JsString::from_utf16(vec![
        9, 10, 11, 12, 13, 32, 0xa0, 0x1680, 0x2000, 0x200a, 0x2028, 0x2029, 0x202f, 0x205f,
        0x3000, 0xfeff,
    ]);
    assert!(
        safe(raw_message(JsValue::Array(vec![block(
            "text",
            JsValue::String(spaces)
        )])))
        .unwrap()
        .is_none()
    );
    for unit in [0x85, 0x180e, 0xd800] {
        assert!(
            safe(raw_message(JsValue::Array(vec![block(
                "text",
                JsValue::String(JsString::from_utf16(vec![unit]))
            )])))
            .unwrap()
            .is_some()
        );
    }
}
#[test]
fn joined_nullish_text_is_empty_without_dropping_fields() {
    for text in [JsValue::Undefined, JsValue::Null] {
        assert!(
            safe(raw_message(JsValue::Array(vec![block("text", text)])))
                .unwrap()
                .is_none()
        );
    }
    assert!(
        safe(raw_message(JsValue::Array(vec![obj([(
            "type",
            "text".into()
        )])])))
        .unwrap()
        .is_none()
    );
}
#[test]
fn number_and_bool_text_source_join_coerces_and_retains_raw() {
    for text in [
        JsValue::Number(0.0),
        JsValue::Bool(false),
        JsValue::Number(f64::INFINITY),
    ] {
        let raw = raw_message(JsValue::Array(vec![block("text", text)]));
        assert_eq!(safe(raw.clone()).unwrap().unwrap().boundary.value(), &raw);
    }
}
#[test]
fn array_text_nested_nullish_join_semantics() {
    assert!(
        safe(raw_message(JsValue::Array(vec![block(
            "text",
            JsValue::Array(vec![JsValue::Null])
        )])))
        .unwrap()
        .is_none()
    );
    assert!(
        safe(raw_message(JsValue::Array(vec![block(
            "text",
            JsValue::Array(vec![JsValue::Null, JsValue::Undefined])
        )])))
        .unwrap()
        .is_some()
    );
    assert!(
        safe(raw_message(JsValue::Array(vec![block(
            "text",
            JsValue::Array(vec![JsValue::Array(vec![JsValue::Null])])
        )])))
        .unwrap()
        .is_none()
    );
}
#[test]
fn ordinary_object_text_is_visible_and_own_noncallable_to_string_throws() {
    assert!(
        safe(raw_message(JsValue::Array(vec![block("text", obj([]))])))
            .unwrap()
            .is_some()
    );
    let error = safe(raw_message(JsValue::Array(vec![block(
        "text",
        obj([("toString", JsValue::Undefined)]),
    )])))
    .unwrap_err();
    assert!(
        matches!(error,PortError::SourceException{message,..} if message.equals_str("Cannot convert object to primitive value"))
    );
}
#[test]
fn thinking_filter_exception_precedes_text_coercion_exception() {
    let error = safe(raw_message(JsValue::Array(vec![
        block("text", obj([("toString", JsValue::Null)])),
        JsValue::Null,
    ])))
    .unwrap_err();
    assert!(
        matches!(error,PortError::SourceException{message,..} if message.equals_str("Cannot read properties of null (reading 'type')"))
    );
}
#[test]
fn malformed_nullish_blocks_raise_original_type_error() {
    for (block, word) in [(JsValue::Null, "null"), (JsValue::Undefined, "undefined")] {
        let error = safe(raw_message(JsValue::Array(vec![block]))).unwrap_err();
        assert!(
            matches!(error,PortError::SourceException{message,..} if message.equals_str(&format!("Cannot read properties of {word} (reading 'type')")))
        );
    }
}
#[test]
fn absent_nonarray_content_is_source_empty_not_invalid() {
    for raw in [
        obj([("role", "assistant".into())]),
        raw_message(JsValue::Null),
        raw_message(JsValue::Undefined),
        raw_message("non-array".into()),
        JsValue::Number(3.0),
    ] {
        assert!(safe(raw).unwrap().is_none());
    }
}
#[test]
fn null_message_without_tools_throws_read_content() {
    assert!(
        matches!(safe(JsValue::Null),Err(PortError::SourceException{message,..}) if message.equals_str("Cannot read properties of null (reading 'content')"))
    );
}
#[test]
fn empty_text_and_thinking_do_not_fabricate_safe_message() {
    assert!(
        safe(raw_message(JsValue::Array(vec![
            block("thinking", "only thinking".into()),
            block("text", "".into())
        ])))
        .unwrap()
        .is_none()
    );
}
#[test]
fn normalized_mismatch_does_not_reclassify_successful_source_boundary() {
    let mut m = project(raw_message(JsValue::Array(vec![block(
        "text",
        "raw text".into(),
    )])));
    m.normalized = MessageProjection::Complete(Box::new(Message {
        role: Role::User,
        metadata: None,
        content: vec![ContentBlock {
            timeline: None,
            body: ContentBody::Text(TextBlock {
                text: "different".into(),
                block_id: None,
            }),
        }],
    }));
    let out = evaluate(&ledger(), BudgetStop::Turn, &m, 0, None)
        .unwrap()
        .safe_message
        .unwrap();
    assert!(matches!(out.normalized, MessageProjection::BoundaryOnly(_)));
    assert_eq!(out.boundary.value(), m.boundary.value());
}
#[test]
fn no_tool_inference_from_raw_content_changes_owner_count() {
    let raw = raw_message(JsValue::Array(vec![
        block("text", "visible".into()),
        obj([("type", "tool_call".into()), ("input", JsValue::Undefined)]),
    ]));
    assert!(safe(raw.clone()).unwrap().is_some());
    assert!(
        evaluate(&ledger(), BudgetStop::Turn, &project(raw), 1, None)
            .unwrap()
            .safe_message
            .is_none()
    );
}
#[test]
fn safe_output_is_detached_from_original_own_extensions() {
    let m = project(raw_message(JsValue::Array(vec![block(
        "text",
        "visible".into(),
    )])));
    let mut out = evaluate(&ledger(), BudgetStop::Turn, &m, 0, None)
        .unwrap()
        .safe_message
        .unwrap()
        .boundary
        .value()
        .clone();
    if let JsValue::Object(ref mut o) = out {
        o.insert("extension".into(), "changed".into());
    }
    assert!(matches!(
        m.boundary.value().get("extension"),
        Some(JsValue::Undefined)
    ));
}
