use pilotdeck_agent::output_recovery::{
    OutputRecovery, OutputRecoveryAction as Action, OutputRecoveryInput, OutputRecoveryPlan,
    OutputRecoverySelection, RecoveryCapEvent,
};
use pilotdeck_agent::ports::{PortError, Route};
use pilotdeck_protocol::model::BoundarySnapshot;
use pilotdeck_protocol::{JsValue, parse_json, stringify_json};
fn b(s: &str) -> BoundarySnapshot {
    BoundarySnapshot::capture(&parse_json(s).unwrap())
}
struct Fixture {
    assembled: BoundarySnapshot,
    message: BoundarySnapshot,
    request: BoundarySnapshot,
    route: Route,
}
impl Fixture {
    fn new(assembled: &str) -> Self {
        Self {
            assembled: b(assembled),
            message: b(
                r#"{"role":"assistant","content":[{"type":"text","text":"already durable"}]}"#,
            ),
            request: b(r#"{"provider":"initial","model":"initial"}"#),
            route: Route {
                provider: "routed\0provider".into(),
                model: "\u{d7ff}".into(),
            },
        }
    }
    fn input(&self, current: Option<f64>, model: Option<f64>) -> OutputRecoveryInput<'_> {
        OutputRecoveryInput {
            assembled: &self.assembled,
            assistant_message: &self.message,
            tool_calls: &[],
            has_repaired_tool_calls: false,
            original_request: &self.request,
            routed_route: &self.route,
            current_max_output_tokens: current,
            routed_model_max_output_tokens: model,
        }
    }
}
fn residual() -> Fixture {
    Fixture::new(r#"{"error":{"code":"max_output_reached"},"finishReason":"error"}"#)
}
fn visible() -> Fixture {
    Fixture::new(r#"{"finishReason":"length"}"#)
}
fn choose(state: &mut OutputRecovery, input: OutputRecoveryInput<'_>) -> OutputRecoveryPlan {
    match state.select(input).unwrap() {
        OutputRecoverySelection::Plan(p) => p,
        OutputRecoverySelection::NeedsLargeFileRepair(t) => t.resolve(None).unwrap(),
    }
}
fn prompt(p: &OutputRecoveryPlan) -> &str {
    match &p.action {
        Action::Continue {
            cap_change: None,
            prompt: Some((text, purpose)),
        } => {
            assert!(purpose.equals_str("max_output_recovery"));
            if text.equals_str("Output token limit hit. Resume directly - no apology, no recap of what you were doing. Pick up mid-sentence if that is where the cut happened."){"short"}else if text.equals_str("Output token limit hit. Resume directly - no apology, no recap of what you were doing. Pick up mid-thought if that is where the cut happened. Break remaining work into smaller pieces."){"long"}else {panic!("unexpected prompt {text:?}")}
        }
        other => panic!("unexpected action {other:?}"),
    }
}
#[test]
fn genuine_defined_bump_strips_error_pair_before_routed_cap_without_prompt() {
    let f = residual();
    let mut s = OutputRecovery::new();
    let p = s
        .select_residual_max_output(f.input(Some(30.0), Some(50.0)))
        .unwrap();
    match &p.action {
        Action::StripErrorPairThenContinue { cap_change } => {
            assert_eq!(cap_change.next, 50.0);
            assert_eq!(cap_change.previous, Some(30.0));
            assert_eq!(cap_change.event, RecoveryCapEvent::TokenCapAdjusted)
        }
        other => panic!("{other:?}"),
    };
    assert_eq!(p.routed_route(), &f.route);
    assert_eq!(s.counters(), (true, false, 0, 0));
}
#[test]
fn unavailable_bump_consumes_guard_before_future_explicit_cap() {
    let f = residual();
    let mut s = OutputRecovery::new();
    assert_eq!(
        prompt(&s.select_residual_max_output(f.input(None, None)).unwrap()),
        "long"
    );
    assert_eq!(
        prompt(
            &s.select_residual_max_output(f.input(Some(10.0), Some(100.0)))
                .unwrap()
        ),
        "long"
    );
    assert_eq!(s.counters(), (true, false, 2, 0));
}
#[test]
fn cap_boundary_matrix_never_invents_same_or_overflow_bump() {
    let f = residual();
    for (current, model) in [
        (Some(0.0), None),
        (Some(-1.0), None),
        (Some(f64::NAN), None),
        (Some(f64::INFINITY), None),
        (Some(1e308), None),
        (Some(5.0), Some(5.0)),
        (Some(5.0), Some(4.0)),
        (Some(5.0), Some(0.0)),
        (Some(5.0), Some(f64::NAN)),
        (Some(5.0), Some(f64::INFINITY)),
    ] {
        let mut s = OutputRecovery::new();
        assert_eq!(
            prompt(
                &s.select_residual_max_output(f.input(current, model))
                    .unwrap()
            ),
            "long"
        );
        assert_eq!(s.counters(), (true, false, 1, 0));
    }
}
#[test]
fn fractional_explicit_bump_is_not_floored_or_defaulted() {
    let f = residual();
    let mut s = OutputRecovery::new();
    let p = s
        .select_residual_max_output(f.input(Some(0.2), Some(0.3)))
        .unwrap();
    assert!(
        matches!(p.action,Action::StripErrorPairThenContinue {cap_change} if cap_change.next==0.3)
    );
}
#[test]
fn strict_other_error_code_does_not_consume_output_attempt() {
    for code in [
        "MAX_OUTPUT_REACHED",
        "prompt_too_long",
        "max_output_reached ",
    ] {
        let f = Fixture::new(&format!(r#"{{"error":{{"code":"{code}"}}}}"#));
        let mut s = OutputRecovery::new();
        assert_eq!(
            s.select_residual_max_output(f.input(Some(1.0), None))
                .unwrap()
                .action,
            Action::FallThrough
        );
        assert_eq!(s.counters(), (false, false, 0, 0));
    }
}
#[test]
fn nullish_error_access_throws_before_flag_with_original_boundary() {
    for raw in ["null", r#"{"error":null}"#, r#"{}"#] {
        let f = Fixture::new(raw);
        let mut s = OutputRecovery::new();
        let e = s
            .select_residual_max_output(f.input(Some(1.0), None))
            .unwrap_err();
        assert!(matches!(e, PortError::SourceException { .. }));
        assert_eq!(s.counters(), (false, false, 0, 0));
    }
}
#[test]
fn visible_text_never_doubles_or_mutates_retry_guards() {
    let f = visible();
    let mut s = OutputRecovery::new();
    let p = s
        .select_visible_length(f.input(Some(10.0), Some(100.0)))
        .unwrap();
    assert_eq!(prompt(&p), "short");
    assert_eq!(s.counters(), (false, false, 1, 0));
    assert!(p.terminal_error().is_err());
    assert!(p.terminal_status(None).is_err());
}
#[test]
fn visible_stage_does_not_revalidate_already_durable_message_or_request() {
    let mut f = visible();
    f.message = b("null");
    f.request = BoundarySnapshot::capture(&JsValue::Undefined);
    let mut s = OutputRecovery::new();
    assert_eq!(
        prompt(
            &s.select_visible_length(f.input(Some(f64::INFINITY), None))
                .unwrap()
        ),
        "short"
    );
    assert_eq!(s.counters(), (false, false, 1, 0));
}
#[test]
fn non_length_visible_stage_preserves_consecutive_empty_count() {
    let mut s = OutputRecovery::new();
    let mut e = Fixture::new(r#"{"finishReason":"stop"}"#);
    e.message = b(r#"{"content":[]}"#);
    choose(&mut s, e.input(None, None));
    choose(&mut s, e.input(None, None));
    assert_eq!(s.counters(), (false, true, 2, 1));
    assert_eq!(
        s.select_visible_length(e.input(None, None)).unwrap().action,
        Action::FallThrough
    );
    assert_eq!(s.counters(), (false, true, 2, 1));
}
#[test]
fn visible_length_resets_consecutive_before_next_empty_without_new_owner() {
    let mut s = OutputRecovery::new();
    let mut e = Fixture::new(r#"{"finishReason":"stop"}"#);
    e.message = b(r#"{"content":[]}"#);
    choose(&mut s, e.input(None, None));
    choose(&mut s, e.input(None, None));
    let v = visible();
    s.select_visible_length(v.input(None, None)).unwrap();
    assert_eq!(s.counters(), (false, true, 3, 0));
    let next = choose(&mut s, e.input(None, None));
    assert!(matches!(next.action, Action::Continue { .. }));
    assert_eq!(s.counters(), (false, true, 4, 1));
}
#[test]
fn reactive_consumption_survives_later_source_failure_and_suppresses_bump() {
    let mut s = OutputRecovery::new();
    assert!(s.consume_reactive_output_attempt());
    let invalid = Fixture::new("null");
    assert!(s.select_visible_length(invalid.input(None, None)).is_err());
    assert!(!s.consume_reactive_output_attempt());
    let f = residual();
    assert_eq!(
        prompt(
            &s.select_residual_max_output(f.input(Some(5.0), Some(100.0)))
                .unwrap()
        ),
        "long"
    );
    assert_eq!(s.counters(), (true, false, 1, 0));
}
#[test]
fn repaired_and_residual_share_one_attempt_and_total_with_visible() {
    let mut s = OutputRecovery::new();
    let f = visible();
    let mut i = f.input(Some(10.0), Some(100.0));
    i.has_repaired_tool_calls = true;
    let p = choose(&mut s, i);
    assert!(matches!(
        p.action,
        Action::Continue {
            cap_change: Some(_),
            prompt: None
        }
    ));
    let f = residual();
    assert_eq!(
        prompt(
            &s.select_residual_max_output(f.input(Some(10.0), Some(100.0)))
                .unwrap()
        ),
        "long"
    );
    let v = visible();
    assert_eq!(
        prompt(&s.select_visible_length(v.input(None, None)).unwrap()),
        "short"
    );
    assert_eq!(s.counters(), (true, false, 2, 0));
}
#[test]
fn fifty_visible_then_residual_falls_through_but_can_still_use_unspent_bump() {
    let v = visible();
    let f = residual();
    let mut s = OutputRecovery::new();
    for _ in 0..50 {
        assert_eq!(
            prompt(&s.select_visible_length(v.input(None, None)).unwrap()),
            "short"
        );
    }
    let p = s
        .select_residual_max_output(f.input(Some(5.0), Some(10.0)))
        .unwrap();
    assert!(matches!(
        p.action,
        Action::StripErrorPairThenContinue { .. }
    ));
    assert_eq!(s.counters(), (true, false, 50, 0));
    assert_eq!(
        s.select_residual_max_output(f.input(None, None))
            .unwrap()
            .action,
        Action::FallThrough
    );
}
#[test]
fn genuine_exhaustion_does_not_become_repaired_error_or_visible_warning() {
    let f = residual();
    let mut s = OutputRecovery::new();
    for _ in 0..50 {
        assert_eq!(
            prompt(&s.select_residual_max_output(f.input(None, None)).unwrap()),
            "long"
        );
    }
    let p = s.select_residual_max_output(f.input(None, None)).unwrap();
    assert_eq!(p.action, Action::FallThrough);
    assert!(p.terminal_error().is_err());
    assert!(p.terminal_status(None).is_err());
}
#[test]
fn visible_exhausted_status_exact_warning_order_and_deferred_inputs() {
    let mut s = OutputRecovery::new();
    let mut v = visible();
    v.request = b("null");
    for _ in 0..50 {
        s.select_visible_length(v.input(None, None)).unwrap();
    }
    let p = s.select_visible_length(v.input(None, None)).unwrap();
    assert_eq!(p.action, Action::VisibleLengthExhausted);
    assert_eq!(s.counters(), (false, false, 50, 0));
    let status = p.terminal_status(Some(&b("null"))).unwrap();
    let expected = b(
        r#"{"event":"max_output_recovery_exhausted","kind":"error","text":"Output token recovery was exhausted, so the visible response may be incomplete. Increase max output tokens or split the task into smaller steps and try again.","detail":{"message":"Output token recovery was exhausted, so the visible response may be incomplete. Increase max output tokens or split the task into smaller steps and try again.","messageI18n":{"key":"chat:agentStatus.maxOutputRecoveryExhausted.message"},"code":"max_output_recovery_exhausted","severity":"warning","visible":true,"userHint":"Increase max output tokens in Settings → Model Provider, or ask the agent to split the answer into smaller parts.","userHintI18n":{"key":"chat:agentStatus.maxOutputRecoveryExhausted.hint"},"scope":"turn","source":"agent","attempts":50}}"#,
    );
    assert_eq!(
        stringify_json(status.value()).unwrap(),
        stringify_json(expected.value()).unwrap()
    );
}
