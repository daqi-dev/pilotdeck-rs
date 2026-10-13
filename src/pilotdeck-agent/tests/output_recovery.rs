use pilotdeck_agent::LargeFileRepairDecision;
use pilotdeck_agent::output_recovery::{
    OutputRecovery, OutputRecoveryAction, OutputRecoveryInput, OutputRecoveryPlan,
    OutputRecoverySelection, RecoveryCapEvent,
};
use pilotdeck_agent::ports::{PortError, Route};
use pilotdeck_protocol::model::BoundarySnapshot;
use pilotdeck_protocol::{JsObject, JsString, JsValue, parse_json};
fn b(s: &str) -> BoundarySnapshot {
    BoundarySnapshot::capture(&parse_json(s).unwrap())
}
fn empty() -> BoundarySnapshot {
    b(r#"{"role":"assistant","content":[]}"#)
}
fn request() -> BoundarySnapshot {
    b(r#"{"provider":"initial-provider","model":"initial-model"}"#)
}
fn route() -> Route {
    Route {
        provider: "routed-provider".into(),
        model: "routed-model".into(),
    }
}
fn assembly(finish: &str) -> BoundarySnapshot {
    b(&format!(r#"{{"finishReason":"{finish}"}}"#))
}
fn input<'a>(
    a: &'a BoundarySnapshot,
    m: &'a BoundarySnapshot,
    calls: &'a [BoundarySnapshot],
    repaired: bool,
    r: &'a BoundarySnapshot,
    route: &'a Route,
    (current, cap): (Option<f64>, Option<f64>),
) -> OutputRecoveryInput<'a> {
    OutputRecoveryInput {
        assembled: a,
        assistant_message: m,
        tool_calls: calls,
        has_repaired_tool_calls: repaired,
        original_request: r,
        routed_route: route,
        current_max_output_tokens: current,
        routed_model_max_output_tokens: cap,
    }
}
fn choose(s: &mut OutputRecovery, i: OutputRecoveryInput<'_>) -> OutputRecoveryPlan {
    match s.select(i).unwrap() {
        OutputRecoverySelection::Plan(p) => p,
        OutputRecoverySelection::NeedsLargeFileRepair(token) => token.resolve(None).unwrap(),
    }
}
fn source_error<T>(r: Result<T, PortError>) {
    assert!(matches!(r, Err(PortError::SourceException { .. })));
}
fn continue_parts(p: &OutputRecoveryPlan) -> (Option<f64>, Option<(JsString, JsString)>) {
    match &p.action {
        OutputRecoveryAction::Continue { cap_change, prompt } => {
            (cap_change.map(|c| c.next), prompt.clone())
        }
        x => panic!("Expected continuation, got {x:?}"),
    }
}
#[test]
fn repaired_selection_preempts_error_and_bad_message_before_any_counter_mutation() {
    let mut s = OutputRecovery::new();
    let a = b(r#"{"finishReason":"stop","error":{"code":"error"}}"#);
    let m = b("null");
    let r = request();
    let route = route();
    {
        let _token = match s
            .select(input(
                &a,
                &m,
                &[],
                true,
                &r,
                &route,
                (Some(100.0), Some(400.0)),
            ))
            .unwrap()
        {
            OutputRecoverySelection::NeedsLargeFileRepair(t) => t,
            _ => panic!(),
        };
    }
    assert_eq!(s.counters(), (false, false, 0, 0));
}
#[test]
fn large_file_decision_preserves_all_counters_and_does_not_strip_dirty_assistant() {
    let mut s = OutputRecovery::new();
    let a = assembly("length");
    let m = empty();
    let r = request();
    let route = route();
    let decision = LargeFileRepairDecision::Continue {
        prompt: "large-owned".into(),
        purpose: "large-file".into(),
        strip: None,
    };
    let token = match s
        .select(input(
            &a,
            &m,
            &[],
            true,
            &r,
            &route,
            (Some(100.0), Some(400.0)),
        ))
        .unwrap()
    {
        OutputRecoverySelection::NeedsLargeFileRepair(t) => t,
        _ => panic!(),
    };
    let p = token.resolve(Some(decision.clone())).unwrap();
    assert_eq!(
        p.action,
        OutputRecoveryAction::UseLargeFileDecision {
            decision,
            strip_current_assistant: false
        }
    );
    assert_eq!(s.counters(), (false, false, 0, 0));
}
#[test]
fn large_file_stop_is_returned_unchanged_without_policy_terminal_effects() {
    let mut s = OutputRecovery::new();
    let a = assembly("stop");
    let m = empty();
    let r = request();
    let route = route();
    let token = match s
        .select(input(&a, &m, &[], true, &r, &route, (None, None)))
        .unwrap()
    {
        OutputRecoverySelection::NeedsLargeFileRepair(t) => t,
        _ => panic!(),
    };
    assert_eq!(
        token
            .resolve(Some(LargeFileRepairDecision::Stop {
                reason: "owned stop".into()
            }))
            .unwrap()
            .action,
        OutputRecoveryAction::UseLargeFileDecision {
            decision: LargeFileRepairDecision::Stop {
                reason: "owned stop".into()
            },
            strip_current_assistant: false
        }
    );
    assert_eq!(s.counters(), (false, false, 0, 0));
}
#[test]
fn repaired_gate_requires_exact_original_finish_labels() {
    for finish in ["length", "tool_call", "stop"] {
        let mut s = OutputRecovery::new();
        let a = assembly(finish);
        let m = empty();
        let r = request();
        let route = route();
        let selected = s
            .select(input(&a, &m, &[], true, &r, &route, (None, None)))
            .unwrap();
        assert!(matches!(
            selected,
            OutputRecoverySelection::NeedsLargeFileRepair(_)
        ));
    }
    for finish in ["content_filter", "custom", "LENGTH", "unknown"] {
        let mut s = OutputRecovery::new();
        let a = assembly(finish);
        let m = b(r#"{"content":[{"type":"text","text":"visible"}]}"#);
        let r = request();
        let route = route();
        assert_eq!(
            choose(&mut s, input(&a, &m, &[], true, &r, &route, (None, None))).action,
            OutputRecoveryAction::FallThrough
        );
    }
}
#[test]
fn token_bump_is_one_shot_without_prompt_or_total_recovery_increment() {
    let mut s = OutputRecovery::new();
    let a = assembly("length");
    let m = empty();
    let r = request();
    let route = route();
    let i = input(&a, &m, &[], true, &r, &route, (Some(100.0), Some(400.0)));
    let p = choose(&mut s, i);
    let OutputRecoveryAction::Continue {
        cap_change: Some(change),
        prompt: None,
    } = p.action
    else {
        panic!()
    };
    assert_eq!(change.event, RecoveryCapEvent::TokenCapAdjusted);
    assert_eq!(change.previous, Some(100.0));
    assert_eq!(change.next, 200.0);
    assert_eq!(s.counters(), (true, false, 0, 0));
    let p = choose(&mut s, i);
    assert!(continue_parts(&p).0.is_none());
    assert_eq!(s.counters(), (true, false, 1, 0));
}
#[test]
fn impossible_bump_consumes_guard_before_continuation_count() {
    let mut s = OutputRecovery::new();
    let a = assembly("length");
    let m = empty();
    let r = request();
    let route = route();
    choose(
        &mut s,
        input(&a, &m, &[], true, &r, &route, (None, Some(100.0))),
    );
    assert_eq!(s.counters(), (true, false, 1, 0));
    let p = choose(
        &mut s,
        input(&a, &m, &[], true, &r, &route, (Some(20.0), Some(100.0))),
    );
    assert!(continue_parts(&p).0.is_none());
    assert_eq!(s.counters(), (true, false, 2, 0));
}
#[test]
fn bump_limits_and_nonfinite_semantics_match_original() {
    let cases = [
        (None, None, None),
        (Some(0.0), None, None),
        (Some(f64::NAN), None, None),
        (Some(f64::INFINITY), None, None),
        (Some(1e308), None, None),
        (Some(8.0), Some(8.0), None),
        (Some(8.0), Some(0.0), None),
        (Some(8.0), Some(f64::INFINITY), None),
        (Some(8.0), Some(f64::NAN), None),
        (Some(8.0), Some(12.0), Some(12.0)),
        (Some(0.4), Some(0.9), Some(0.8)),
        (Some(8.0), None, Some(16.0)),
    ];
    for (current, cap, expected) in cases {
        let mut s = OutputRecovery::new();
        let a = assembly("length");
        let m = empty();
        let r = request();
        let route = route();
        assert_eq!(
            continue_parts(&choose(
                &mut s,
                input(&a, &m, &[], true, &r, &route, (current, cap))
            ))
            .0,
            expected
        );
    }
}
#[test]
fn repaired_continuation_reaches_fifty_then_exhausts_without_dirty_text() {
    let mut s = OutputRecovery::new();
    let a = assembly("stop");
    let m = empty();
    let r = request();
    let route = route();
    let i = input(&a, &m, &[], true, &r, &route, (None, None));
    for count in 1..=50 {
        let p = choose(&mut s, i);
        assert!(matches!(p.action, OutputRecoveryAction::Continue { .. }));
        assert_eq!(s.counters(), (true, false, count, 0));
    }
    let p = choose(&mut s, i);
    assert_eq!(p.action, OutputRecoveryAction::RepairedExhausted);
    assert_eq!(s.counters(), (true, false, 50, 0));
    let e = p.terminal_error().unwrap();
    assert_eq!(e.value().get("details"), Some(&JsValue::Undefined));
    assert_eq!(e.value().get("userHint"), None);
    assert_eq!(
        e.value().get("message"),
        Some(&JsValue::from(
            "Recovered tool call still looked repaired/truncated after max-output recovery was exhausted."
        ))
    );
    let status = p.terminal_status(Some(&e)).unwrap();
    assert_eq!(
        status.value().get("event"),
        Some(&JsValue::from("tool_call_recovery_exhausted"))
    );
    assert_eq!(
        status.value().get("detail").unwrap().get("attempts"),
        Some(&JsValue::Number(50.0))
    );
}
#[test]
fn exact_repaired_and_first_empty_prompts_are_distinct() {
    let mut s = OutputRecovery::new();
    let a = assembly("stop");
    let m = empty();
    let r = request();
    let route = route();
    let p = choose(&mut s, input(&a, &m, &[], true, &r, &route, (None, None)));
    let (prompt, purpose) = continue_parts(&p).1.unwrap();
    assert!(prompt.equals_str("Output token limit hit. Resume directly - no apology, no recap of what you were doing. Pick up mid-thought if that is where the cut happened. Break remaining work into smaller pieces."));
    assert!(purpose.equals_str("max_output_recovery"));
    let p = choose(
        &mut OutputRecovery::new(),
        input(&a, &m, &[], false, &r, &route, (None, None)),
    );
    let (prompt, purpose) = continue_parts(&p).1.unwrap();
    assert!(prompt.equals_str("Your previous response was empty (thinking only, no visible text). Please provide your answer as visible text output."));
    assert!(purpose.equals_str("empty_response_retry"));
}
#[test]
fn first_empty_then_two_continuations_then_third_consecutive_exhaustion() {
    let mut s = OutputRecovery::new();
    let a = assembly("stop");
    let m = empty();
    let r = request();
    let route = route();
    let i = input(&a, &m, &[], false, &r, &route, (None, None));
    choose(&mut s, i);
    assert_eq!(s.counters(), (false, true, 1, 0));
    for count in 1..=2 {
        let p = choose(&mut s, i);
        let (prompt, purpose) = continue_parts(&p).1.unwrap();
        assert!(prompt.equals_str("Output token limit hit. Resume directly - no apology, no recap of what you were doing. Pick up mid-sentence if that is where the cut happened."));
        assert!(purpose.equals_str("max_output_recovery"));
        assert_eq!(s.counters(), (false, true, count + 1, count));
    }
    let p = choose(&mut s, i);
    assert_eq!(
        p.action,
        OutputRecoveryAction::EmptyExhausted { attempts: 3 }
    );
    assert_eq!(s.counters(), (false, true, 3, 3));
    assert!(matches!(
        p.terminal_error(),
        Err(PortError::InvalidResponse { .. })
    ));
}
#[test]
fn empty_after_repaired_continuation_uses_total_branch_without_first_flag() {
    let mut s = OutputRecovery::new();
    let a = assembly("stop");
    let m = empty();
    let r = request();
    let route = route();
    choose(&mut s, input(&a, &m, &[], true, &r, &route, (None, None)));
    let p = choose(&mut s, input(&a, &m, &[], false, &r, &route, (None, None)));
    assert!(
        continue_parts(&p)
            .1
            .unwrap()
            .1
            .equals_str("max_output_recovery")
    );
    assert_eq!(s.counters(), (true, false, 2, 1));
}
#[test]
fn empty_after_bump_only_still_is_first_empty_branch() {
    let mut s = OutputRecovery::new();
    let a = assembly("length");
    let m = empty();
    let r = request();
    let route = route();
    choose(
        &mut s,
        input(&a, &m, &[], true, &r, &route, (Some(100.0), Some(400.0))),
    );
    let p = choose(
        &mut s,
        input(&a, &m, &[], false, &r, &route, (Some(200.0), Some(400.0))),
    );
    assert!(
        continue_parts(&p)
            .1
            .unwrap()
            .1
            .equals_str("empty_response_retry")
    );
    assert_eq!(s.counters(), (true, true, 1, 0));
}
#[test]
fn no_fallthrough_reset_of_any_of_four_state_fields() {
    let mut s = OutputRecovery::new();
    let a = assembly("stop");
    let m = empty();
    let r = request();
    let route = route();
    choose(&mut s, input(&a, &m, &[], true, &r, &route, (None, None)));
    choose(&mut s, input(&a, &m, &[], false, &r, &route, (None, None)));
    let before = s.counters();
    let visible = b(r#"{"content":[{"type":"text","text":"visible"}]}"#);
    assert_eq!(
        choose(
            &mut s,
            input(&a, &visible, &[], false, &r, &route, (None, None))
        )
        .action,
        OutputRecoveryAction::FallThrough
    );
    assert_eq!(s.counters(), before);
}
#[test]
fn whitespace_and_multiple_nullish_texts_are_not_empty() {
    let a = assembly("stop");
    let r = request();
    let route = route();
    for m in [
        b(r#"{"content":[{"type":"text","text":" "}]}"#),
        b(r#"{"content":[{"type":"text","text":null},{"type":"text"}]}"#),
        b(r#"{"content":[{"type":"text","text":"\ud800"}]}"#),
    ] {
        assert_eq!(
            choose(
                &mut OutputRecovery::new(),
                input(&a, &m, &[], false, &r, &route, (None, None))
            )
            .action,
            OutputRecoveryAction::FallThrough
        );
    }
}
#[test]
fn thinking_only_and_single_nullish_text_are_empty() {
    let a = assembly("stop");
    let r = request();
    let route = route();
    for m in [
        b(r#"{"content":[{"type":"thinking","thinking":"hidden"}]}"#),
        b(r#"{"content":[{"type":"text","text":null}]}"#),
        b(r#"{"content":[{"type":"text"}]}"#),
    ] {
        assert!(matches!(
            choose(
                &mut OutputRecovery::new(),
                input(&a, &m, &[], false, &r, &route, (None, None))
            )
            .action,
            OutputRecoveryAction::Continue { .. }
        ));
    }
}
#[test]
fn numeric_boolean_and_object_text_are_join_coerced_not_emptied() {
    let a = assembly("stop");
    let r = request();
    let route = route();
    for text in ["0", "false", "{}", "[null,1,null]"] {
        let m = b(&format!(
            r#"{{"content":[{{"type":"text","text":{text}}}]}}"#
        ));
        assert_eq!(
            choose(
                &mut OutputRecovery::new(),
                input(&a, &m, &[], false, &r, &route, (None, None))
            )
            .action,
            OutputRecoveryAction::FallThrough
        );
    }
}
#[test]
fn empty_predicates_shortcircuit_bad_message_when_error_or_calls_exist() {
    let a = b(r#"{"error":{}}"#);
    let m = b("null");
    let r = request();
    let route = route();
    assert_eq!(
        choose(
            &mut OutputRecovery::new(),
            input(&a, &m, &[], false, &r, &route, (None, None))
        )
        .action,
        OutputRecoveryAction::FallThrough
    );
    let a = assembly("stop");
    assert_eq!(
        choose(
            &mut OutputRecovery::new(),
            input(&a, &m, &[b("{}")], false, &r, &route, (None, None))
        )
        .action,
        OutputRecoveryAction::FallThrough
    );
}
#[test]
fn malformed_empty_content_is_source_exception_not_message_content_fallback() {
    let a = assembly("stop");
    let r = request();
    let route = route();
    for m in [
        b("null"),
        b("{}"),
        b(r#"{"content":null}"#),
        b(r#"{"content":"x"}"#),
        b(r#"{"content":[null]}"#),
        b(r#"{"content":[{"type":"text","text":{"toString":null}}]}"#),
    ] {
        let mut s = OutputRecovery::new();
        source_error(s.select(input(&a, &m, &[], false, &r, &route, (None, None))));
        assert_eq!(s.counters(), (false, false, 0, 0));
    }
}
#[test]
fn full_filter_precedes_text_join_coercion() {
    let a = assembly("stop");
    let m = b(r#"{"content":[{"type":"text","text":{"toString":null}},null]}"#);
    let r = request();
    let route = route();
    let error = OutputRecovery::new()
        .select(input(&a, &m, &[], false, &r, &route, (None, None)))
        .unwrap_err();
    let PortError::SourceException { message, .. } = error else {
        panic!()
    };
    assert!(message.equals_str("Cannot read properties of null (reading 'type')"));
}
#[test]
fn empty_length_clamp_differs_from_repaired_bump_and_preserves_defined_zero() {
    let a = assembly("length");
    let m = empty();
    let r = request();
    let route = route();
    for (current, cap, expected) in [
        (None, None, Some(4096.0)),
        (Some(10.0), Some(1000.9), Some(1000.0)),
        (Some(5000.5), None, Some(10001.0)),
        (None, Some(0.9), Some(0.0)),
        (Some(0.0), Some(0.9), None),
        (Some(f64::NAN), None, None),
        (Some(f64::INFINITY), None, None),
        (Some(f64::NEG_INFINITY), None, Some(4096.0)),
        (None, Some(f64::INFINITY), Some(4096.0)),
        (None, Some(-1.0), Some(4096.0)),
    ] {
        let p = choose(
            &mut OutputRecovery::new(),
            input(&a, &m, &[], false, &r, &route, (current, cap)),
        );
        assert_eq!(continue_parts(&p).0, expected);
        if let OutputRecoveryAction::Continue {
            cap_change: Some(change),
            ..
        } = p.action
        {
            assert_eq!(change.event, RecoveryCapEvent::EmptyOutputRecovery);
        }
    }
}
#[test]
fn empty_cap_event_only_when_defined_and_not_equal_to_previous() {
    let a = assembly("length");
    let m = empty();
    let r = request();
    let route = route();
    assert!(
        continue_parts(&choose(
            &mut OutputRecovery::new(),
            input(&a, &m, &[], false, &r, &route, (Some(4096.0), Some(4096.0)))
        ))
        .0
        .is_none()
    );
    let a = assembly("stop");
    assert!(
        continue_parts(&choose(
            &mut OutputRecovery::new(),
            input(&a, &m, &[], false, &r, &route, (None, None))
        ))
        .0
        .is_none()
    );
}
#[test]
fn total_fifty_preempts_consecutive_limit_and_increments_consecutive_first() {
    let mut s = OutputRecovery::new();
    let a = assembly("stop");
    let m = empty();
    let r = request();
    let route = route();
    for _ in 0..50 {
        choose(&mut s, input(&a, &m, &[], true, &r, &route, (None, None)));
    }
    let p = choose(&mut s, input(&a, &m, &[], false, &r, &route, (None, None)));
    assert_eq!(
        p.action,
        OutputRecoveryAction::EmptyExhausted { attempts: 1 }
    );
    assert_eq!(s.counters(), (true, false, 50, 1));
}
#[test]
fn empty_status_uses_initial_request_and_retains_lax_null_utf16() {
    let mut request = JsObject::new();
    request.insert("provider".into(), JsValue::Null);
    request.insert(
        "model".into(),
        JsValue::String(JsString::from_utf16(vec![0xd800])),
    );
    let request = BoundarySnapshot::capture(&JsValue::Object(request));
    let a = assembly("stop");
    let m = empty();
    let route = route();
    let mut s = OutputRecovery::new();
    for _ in 0..3 {
        choose(
            &mut s,
            input(&a, &m, &[], false, &request, &route, (None, None)),
        );
    }
    let p = choose(
        &mut s,
        input(&a, &m, &[], false, &request, &route, (None, None)),
    );
    let status = p.terminal_status(None).unwrap();
    assert_eq!(
        status.value().get("event"),
        Some(&JsValue::from("model_empty_response_exhausted"))
    );
    let detail = status.value().get("detail").unwrap();
    assert_eq!(detail.get("provider"), Some(&JsValue::Null));
    assert_eq!(detail.get("model"), request.value().get("model"));
    assert_eq!(detail.get("attempts"), Some(&JsValue::Number(3.0)));
    assert_eq!(detail.get("severity"), Some(&JsValue::from("error")));
    assert_eq!(detail.get("visible"), Some(&JsValue::Bool(true)));
}
#[test]
fn empty_status_prunes_only_top_undefined_and_defers_bad_request_until_status() {
    let a = assembly("stop");
    let m = empty();
    let route = route();
    let req = b("{}");
    let mut s = OutputRecovery::new();
    for _ in 0..3 {
        choose(
            &mut s,
            input(&a, &m, &[], false, &req, &route, (None, None)),
        );
    }
    let p = choose(
        &mut s,
        input(&a, &m, &[], false, &req, &route, (None, None)),
    );
    let status = p.terminal_status(None).unwrap();
    assert_eq!(status.value().get("detail").unwrap().get("provider"), None);
    let req = b("null");
    let mut s = OutputRecovery::new();
    for _ in 0..3 {
        choose(
            &mut s,
            input(&a, &m, &[], false, &req, &route, (None, None)),
        );
    }
    let p = choose(
        &mut s,
        input(&a, &m, &[], false, &req, &route, (None, None)),
    );
    source_error(p.terminal_status(None));
    assert_eq!(s.counters(), (false, true, 3, 3));
}
#[test]
fn repaired_status_keeps_nonstring_truthy_message_and_nullish_hint_rules() {
    let mut s = OutputRecovery::new();
    let a = assembly("stop");
    let m = empty();
    let req = request();
    let route = route();
    for _ in 0..50 {
        choose(&mut s, input(&a, &m, &[], true, &req, &route, (None, None)));
    }
    let p = choose(&mut s, input(&a, &m, &[], true, &req, &route, (None, None)));
    let error = b(r#"{"message":{"owned":true},"code":null,"userHint":""}"#);
    let status = p.terminal_status(Some(&error)).unwrap();
    assert_eq!(status.value().get("text"), error.value().get("message"));
    let detail = status.value().get("detail").unwrap();
    assert_eq!(detail.get("code"), Some(&JsValue::Null));
    assert_eq!(detail.get("userHint"), Some(&JsValue::from("")));
    assert_eq!(
        detail.get("reason"),
        Some(&JsValue::from("repaired_truncated_tool_calls"))
    );
    source_error(p.terminal_status(Some(&b("null"))));
}
#[test]
fn source_error_falsy_null_and_undefined_allow_empty_retry() {
    let a = assembly("stop");
    let m = empty();
    let req = request();
    let route = route();
    for error in [
        JsValue::Null,
        JsValue::Undefined,
        JsValue::Bool(false),
        JsValue::Number(f64::NAN),
        JsValue::Number(-0.0),
    ] {
        let mut assembled = a.value().as_object().unwrap().clone();
        assembled.insert("error".into(), error);
        let a = BoundarySnapshot::capture(&JsValue::Object(assembled));
        assert!(matches!(
            choose(
                &mut OutputRecovery::new(),
                input(&a, &m, &[], false, &req, &route, (None, None))
            )
            .action,
            OutputRecoveryAction::Continue { .. }
        ));
    }
}
