use pilotdeck_agent::early_stream_recovery::{
    EarlyStreamAction, EarlyStreamInput, EarlyStreamKind, EarlyStreamPlan, EarlyStreamRecovery,
};
use pilotdeck_agent::ports::PortError;
use pilotdeck_protocol::model::BoundarySnapshot;
use pilotdeck_protocol::{JsObject, JsString, JsValue, parse_json};
fn b(s: &str) -> BoundarySnapshot {
    BoundarySnapshot::capture(&parse_json(s).unwrap())
}
fn v(s: &str) -> JsValue {
    parse_json(s).unwrap()
}
fn capture(value: JsValue) -> BoundarySnapshot {
    BoundarySnapshot::capture(&value)
}
fn select(
    state: &mut EarlyStreamRecovery,
    assembled: &BoundarySnapshot,
    message: &BoundarySnapshot,
    calls: &[BoundarySnapshot],
    end: bool,
) -> EarlyStreamPlan {
    state
        .select(EarlyStreamInput {
            assembled,
            assistant_message: message,
            tool_calls: calls,
            has_message_end: end,
        })
        .unwrap()
}
fn msg() -> BoundarySnapshot {
    b(r#"{"role":"assistant","content":[{"type":"text","text":"hello"}]}"#)
}
fn stream(phase: &str) -> BoundarySnapshot {
    b(&format!(
        r#"{{"error":{{"streamInterruption":{{"phase":"{phase}"}}}}}}"#
    ))
}
fn exhausted(
    assembled: &BoundarySnapshot,
    message: &BoundarySnapshot,
    calls: &[BoundarySnapshot],
) -> EarlyStreamPlan {
    let mut state = EarlyStreamRecovery::new();
    select(&mut state, assembled, message, calls, true);
    select(&mut state, assembled, message, calls, true);
    select(&mut state, assembled, message, calls, true)
}
fn source_error<T>(result: Result<T, PortError>) {
    assert!(matches!(result, Err(PortError::SourceException { .. })));
}
#[test]
fn text_interruption_retries_twice_then_exhausts() {
    let mut s = EarlyStreamRecovery::new();
    let a = stream("text");
    let m = msg();
    for count in 1..=2 {
        let p = select(&mut s, &a, &m, &[], true);
        assert_eq!(
            p.action,
            EarlyStreamAction::Retry {
                clear_final_message: false,
                retain_partial_text: true
            }
        );
        assert_eq!(s.counters(), (count, 0));
        assert_eq!(
            p.partial_text_message().unwrap().unwrap().value(),
            m.value()
        );
    }
    let p = select(&mut s, &a, &m, &[], true);
    assert_eq!(p.action, EarlyStreamAction::Exhausted);
    assert_eq!(s.counters(), (2, 0));
    assert_eq!(
        p.terminal_error().unwrap().value().get("message"),
        Some(&JsValue::from(
            "Stream interruption recovery exhausted after 2 attempts (text)."
        ))
    );
}
#[test]
fn unknown_retries_twice_then_exhausts_and_owns_undefined_details() {
    let a = b(r#"{"finishReason":"unknown"}"#);
    let m = msg();
    let mut s = EarlyStreamRecovery::new();
    assert!(matches!(
        select(&mut s, &a, &m, &[], true).action,
        EarlyStreamAction::Retry { .. }
    ));
    assert!(matches!(
        select(&mut s, &a, &m, &[], true).action,
        EarlyStreamAction::Retry { .. }
    ));
    let p = select(&mut s, &a, &m, &[], true);
    assert_eq!(p.action, EarlyStreamAction::Exhausted);
    assert_eq!(s.counters(), (0, 2));
    let e = p.terminal_error().unwrap();
    assert_eq!(e.value().get("details"), Some(&JsValue::Undefined));
    assert_eq!(
        e.value().get("message"),
        Some(&JsValue::from(
            "Unknown finish reason recovery exhausted after 2 attempts."
        ))
    );
}
#[test]
fn interruption_preempts_unknown_without_resetting_its_counter() {
    let mut s = EarlyStreamRecovery::new();
    let m = msg();
    let u = b(r#"{"finishReason":"unknown"}"#);
    select(&mut s, &u, &m, &[], true);
    let p = select(&mut s, &stream("reasoning"), &m, &[], true);
    assert_eq!(p.kind, Some(EarlyStreamKind::Interruption));
    assert_eq!(s.counters(), (1, 1));
    select(&mut s, &u, &m, &[], true);
    assert_eq!(s.counters(), (0, 2));
}
#[test]
fn nonmatching_gate_resets_only_when_reached() {
    let mut s = EarlyStreamRecovery::new();
    let m = msg();
    select(&mut s, &stream("text"), &m, &[], true);
    select(&mut s, &b(r#"{"finishReason":"unknown"}"#), &m, &[], true);
    let p = select(&mut s, &b(r#"{"finishReason":"stop"}"#), &m, &[], true);
    assert_eq!(p.action, EarlyStreamAction::FallThrough);
    assert_eq!(s.counters(), (0, 0));
}
#[test]
fn custom_finish_and_missing_end_never_use_unknown_recovery() {
    for a in [
        b(r#"{"finishReason":"UNKNOWN"}"#),
        b(r#"{"finishReason":"custom"}"#),
        b(r#"{"finishReason":null}"#),
        b(r#"{"finishReason":1}"#),
    ] {
        assert_eq!(
            select(&mut EarlyStreamRecovery::new(), &a, &msg(), &[], true).action,
            EarlyStreamAction::FallThrough
        );
    }
    assert_eq!(
        select(
            &mut EarlyStreamRecovery::new(),
            &b(r#"{"finishReason":"unknown"}"#),
            &msg(),
            &[],
            false
        )
        .action,
        EarlyStreamAction::FallThrough
    );
}
#[test]
fn source_error_truthiness_controls_unknown_gate() {
    for error in [
        JsValue::Undefined,
        JsValue::Null,
        JsValue::Bool(false),
        JsValue::Number(0.0),
        JsValue::Number(f64::NAN),
        JsValue::from(""),
    ] {
        let mut a = JsObject::new();
        a.insert("error".into(), error);
        a.insert("finishReason".into(), "unknown".into());
        assert_eq!(
            select(
                &mut EarlyStreamRecovery::new(),
                &capture(JsValue::Object(a)),
                &msg(),
                &[],
                true
            )
            .kind,
            Some(EarlyStreamKind::UnknownFinish)
        );
    }
    for error in [
        v("{}"),
        v("[]"),
        JsValue::Bool(true),
        JsValue::from("error"),
    ] {
        let mut a = JsObject::new();
        a.insert("error".into(), error);
        a.insert("finishReason".into(), "unknown".into());
        assert_eq!(
            select(
                &mut EarlyStreamRecovery::new(),
                &capture(JsValue::Object(a)),
                &msg(),
                &[],
                true
            )
            .action,
            EarlyStreamAction::FallThrough
        );
    }
}
#[test]
fn primitive_truthy_interruption_preserves_lax_source_branch() {
    let a = b(r#"{"error":{"streamInterruption":true}}"#);
    let p = select(&mut EarlyStreamRecovery::new(), &a, &msg(), &[], true);
    assert_eq!(
        p.action,
        EarlyStreamAction::Retry {
            clear_final_message: false,
            retain_partial_text: false
        }
    );
    assert!(p.retry_prompt().unwrap().0.equals_str("The previous model stream disconnected before producing a response. Continue the original task directly from the current workspace state."));
    let p = exhausted(&a, &msg(), &[]);
    assert!(
        p.terminal_error()
            .unwrap()
            .value()
            .get("message")
            .unwrap()
            .as_string()
            .unwrap()
            .equals_str("Stream interruption recovery exhausted after 2 attempts (undefined).")
    );
}
#[test]
fn structured_interruption_clears_but_unknown_does_not() {
    let call = b(r#"{"name":"write_file"}"#);
    let m = msg();
    let p = select(
        &mut EarlyStreamRecovery::new(),
        &stream("text"),
        &m,
        std::slice::from_ref(&call),
        true,
    );
    assert_eq!(
        p.action,
        EarlyStreamAction::Retry {
            clear_final_message: true,
            retain_partial_text: false
        }
    );
    assert!(p.partial_text_message().unwrap().is_none());
    let p = select(
        &mut EarlyStreamRecovery::new(),
        &b(r#"{"finishReason":"unknown"}"#),
        &m,
        &[call],
        true,
    );
    assert_eq!(
        p.action,
        EarlyStreamAction::Retry {
            clear_final_message: false,
            retain_partial_text: false
        }
    );
}
#[test]
fn explicit_tool_phase_clears_even_when_calls_empty() {
    let p = select(
        &mut EarlyStreamRecovery::new(),
        &stream("tool_call"),
        &msg(),
        &[],
        true,
    );
    assert_eq!(
        p.action,
        EarlyStreamAction::Retry {
            clear_final_message: true,
            retain_partial_text: false
        }
    );
}
#[test]
fn unknown_structured_retry_still_projects_without_thinking_first() {
    let a = b(r#"{"finishReason":"unknown"}"#);
    let bad = b(r#"{"content":[null]}"#);
    let calls = [b("{}")];
    let mut s = EarlyStreamRecovery::new();
    let p = select(&mut s, &a, &bad, &calls, true);
    source_error(p.partial_text_message());
    assert_eq!(s.counters(), (0, 1));
    let p = select(
        &mut EarlyStreamRecovery::new(),
        &stream("tool_call"),
        &bad,
        &calls,
        true,
    );
    assert!(p.partial_text_message().unwrap().is_none());
}
#[test]
fn unknown_structured_retry_shortcircuits_text_conversion_after_filter() {
    let m = b(r#"{"content":[{"type":"text","text":{"toString":null}}]}"#);
    let p = select(
        &mut EarlyStreamRecovery::new(),
        &b(r#"{"finishReason":"unknown"}"#),
        &m,
        &[b("{}")],
        true,
    );
    assert!(p.partial_text_message().unwrap().is_none());
}
#[test]
fn exhaustion_structured_shortcircuits_all_safe_message_access() {
    let p = exhausted(&stream("tool_call"), &capture(JsValue::Null), &[b("{}")]);
    assert!(p.terminal_error().is_ok());
    assert!(p.partial_text_message().unwrap().is_none());
}
#[test]
fn partial_message_keeps_media_extensions_and_raw_utf16() {
    let mut block = JsObject::new();
    block.insert("type".into(), "text".into());
    block.insert(
        "text".into(),
        JsValue::String(JsString::from_utf16(vec![0xd800, 0x78])),
    );
    block.insert("extension".into(), JsValue::Undefined);
    let mut m = JsObject::new();
    m.insert("role".into(), "assistant".into());
    m.insert("unknown".into(), JsValue::Null);
    m.insert(
        "content".into(),
        JsValue::Array(vec![
            v(r#"{"type":"thinking","thinking":"hidden"}"#),
            JsValue::Object(block.clone()),
            v(r#"{"type":"image","source":{"type":"url","url":"owned"},"extra":1}"#),
        ]),
    );
    let m = capture(JsValue::Object(m));
    let p = select(
        &mut EarlyStreamRecovery::new(),
        &stream("text"),
        &m,
        &[],
        true,
    );
    let out = p.partial_text_message().unwrap().unwrap();
    assert_eq!(out.value().get("unknown"), Some(&JsValue::Null));
    let JsValue::Array(content) = out.value().get("content").unwrap() else {
        panic!()
    };
    assert_eq!(content.len(), 2);
    assert_eq!(content[0], JsValue::Object(block));
    assert_eq!(m.value().get("content").unwrap().as_object(), None);
}
#[test]
fn source_text_array_join_nullish_numeric_object_coercion() {
    for text in [
        JsValue::Number(0.0),
        v("{}"),
        v("[null,1,null]"),
        JsValue::Bool(false),
    ] {
        let mut block = JsObject::new();
        block.insert("type".into(), "text".into());
        block.insert("text".into(), text);
        let mut m = JsObject::new();
        m.insert(
            "content".into(),
            JsValue::Array(vec![JsValue::Object(block)]),
        );
        assert!(
            select(
                &mut EarlyStreamRecovery::new(),
                &stream("text"),
                &capture(JsValue::Object(m)),
                &[],
                true
            )
            .partial_text_message()
            .unwrap()
            .is_some()
        );
    }
    let m = b(
        r#"{"content":[{"type":"text"},{"type":"text","text":null},{"type":"text","text":"\ufeff \n"}]}"#,
    );
    assert!(
        select(
            &mut EarlyStreamRecovery::new(),
            &stream("text"),
            &m,
            &[],
            true
        )
        .partial_text_message()
        .unwrap()
        .is_none()
    );
}
#[test]
fn source_trim_keeps_zero_width_space_and_removes_ecma_whitespace() {
    let m = b(r#"{"content":[{"type":"text","text":"\u200b"}]}"#);
    assert!(
        select(
            &mut EarlyStreamRecovery::new(),
            &stream("text"),
            &m,
            &[],
            true
        )
        .partial_text_message()
        .unwrap()
        .is_some()
    );
}
#[test]
fn source_filter_rejects_null_block_without_silently_normalizing() {
    let mut s = EarlyStreamRecovery::new();
    let p = select(
        &mut s,
        &stream("text"),
        &b(r#"{"content":[null]}"#),
        &[],
        true,
    );
    source_error(p.partial_text_message());
    assert_eq!(s.counters(), (1, 0));
}
#[test]
fn nonarray_content_uses_original_message_content_empty_fallback() {
    for m in [
        b(r#"{"content":null}"#),
        b(r#"{"content":"abc"}"#),
        b("{}"),
        capture(JsValue::Number(2.0)),
    ] {
        assert!(
            select(
                &mut EarlyStreamRecovery::new(),
                &stream("text"),
                &m,
                &[],
                true
            )
            .partial_text_message()
            .unwrap()
            .is_none()
        );
    }
}
#[test]
fn malformed_assembled_null_throws_before_any_counter_changes() {
    let mut s = EarlyStreamRecovery::new();
    let a = capture(JsValue::Null);
    let m = msg();
    source_error(s.select(EarlyStreamInput {
        assembled: &a,
        assistant_message: &m,
        tool_calls: &[],
        has_message_end: true,
    }));
    assert_eq!(s.counters(), (0, 0));
}
#[test]
fn structured_clear_and_late_prompt_error_have_distinct_phases() {
    let a = b(r#"{"error":{"streamInterruption":{"phase":"tool_call","activeToolCalls":1}}}"#);
    let mut s = EarlyStreamRecovery::new();
    let p = select(&mut s, &a, &msg(), &[], true);
    assert_eq!(
        p.action,
        EarlyStreamAction::Retry {
            clear_final_message: true,
            retain_partial_text: false
        }
    );
    assert!(p.partial_text_message().unwrap().is_none());
    source_error(p.retry_prompt());
    assert_eq!(s.counters(), (1, 0));
}
#[test]
fn tool_prompt_maps_all_calls_before_slice_but_coerces_only_three_names() {
    let a = b(
        r#"{"error":{"streamInterruption":{"phase":"tool_call","activeToolCalls":[{"name":"a"},{"name":0},{"name":true},{"name":{"toString":null}}]}}}"#,
    );
    let p = select(&mut EarlyStreamRecovery::new(), &a, &msg(), &[], true);
    assert!(p.retry_prompt().unwrap().0.to_utf8().unwrap().starts_with(
        "The previous model stream disconnected while generating a tool call (a, unknown, true)."
    ));
    let a = b(
        r#"{"error":{"streamInterruption":{"phase":"tool_call","activeToolCalls":[{},{},{},null]}}}"#,
    );
    source_error(select(&mut EarlyStreamRecovery::new(), &a, &msg(), &[], true).retry_prompt());
}
#[test]
fn all_prompt_literals_and_kinds_match_fixed_source() {
    let examples = [
        (
            "reasoning",
            "The previous model stream disconnected during reasoning. Continue the original task directly from the current workspace state; do not repeat analysis or recap.",
        ),
        (
            "text",
            "The previous model stream disconnected mid-response. Continue exactly where the visible response ended; do not repeat prior text or recap.",
        ),
        (
            "custom",
            "The previous model stream disconnected before producing a response. Continue the original task directly from the current workspace state.",
        ),
    ];
    for (phase, expected) in examples {
        let (prompt, kind) = select(
            &mut EarlyStreamRecovery::new(),
            &stream(phase),
            &msg(),
            &[],
            true,
        )
        .retry_prompt()
        .unwrap();
        assert!(prompt.equals_str(expected));
        assert!(kind.equals_str("stream_interruption_recovery"));
    }
    let (prompt, kind) = select(
        &mut EarlyStreamRecovery::new(),
        &b(r#"{"finishReason":"unknown"}"#),
        &msg(),
        &[],
        true,
    )
    .retry_prompt()
    .unwrap();
    assert!(prompt.equals_str("The previous response ended without a recognized finish reason. Continue exactly where the visible response ended; do not repeat prior text or recap."));
    assert!(kind.equals_str("unknown_finish_recovery"));
}
#[test]
fn terminal_error_retains_full_model_error_extensions_and_details() {
    let a = b(r#"{"error":{"streamInterruption":{"phase":"text"},"extension":{"raw":null}}}"#);
    let e = exhausted(&a, &msg(), &[]).terminal_error().unwrap();
    assert_eq!(e.value().get("details"), a.value().get("error"));
    assert_eq!(
        e.value().get("userHint"),
        Some(&JsValue::from(
            "The model stream repeatedly disconnected. Retry the turn or switch providers."
        ))
    );
}
#[test]
fn terminal_error_phase_primitive_conversion_precedes_safe_message() {
    let a = b(r#"{"error":{"streamInterruption":{"phase":{"toString":null}}}}"#);
    let p = exhausted(&a, &capture(JsValue::Null), &[]);
    source_error(p.terminal_error());
    source_error(p.partial_text_message());
}
#[test]
fn late_status_error_occurs_after_successful_safe_message_projection() {
    let a = b(r#"{"error":{"streamInterruption":{"phase":"text"},"provider":{"toString":null}}}"#);
    let p = exhausted(&a, &msg(), &[]);
    let e = p.terminal_error().unwrap();
    assert!(p.partial_text_message().unwrap().is_some());
    source_error(p.terminal_status(&e));
    assert_eq!(
        e.value().get("code"),
        Some(&JsValue::from("agent_model_error"))
    );
}
#[test]
fn unknown_status_default_has_shallow_pruning_and_fixed_action() {
    let p = exhausted(&b(r#"{"finishReason":"unknown"}"#), &msg(), &[]);
    let e = p.terminal_error().unwrap();
    let s = p.terminal_status(&e).unwrap();
    let d = s.value().get("detail").unwrap();
    assert_eq!(d.get("severity"), Some(&JsValue::from("error")));
    assert_eq!(d.get("visible"), Some(&JsValue::Bool(true)));
    assert_eq!(d.get("scope"), Some(&JsValue::from("turn")));
    assert_eq!(d.get("provider"), None);
    assert_eq!(d.get("fixTarget"), Some(&JsValue::from("settings")));
    assert!(
        s.value()
            .get("text")
            .unwrap()
            .as_string()
            .unwrap()
            .to_utf8()
            .unwrap()
            .contains("Action: Check Settings → Model Provider and verify")
    );
}
#[test]
fn model_status_all_action_branches_follow_source_precedence() {
    for (code, status, target, key) in [
        ("other", 401, "settings", "auth"),
        ("model_not_found", 0, "settings", "modelNotFound"),
        ("timeout", 0, "network", "timeout"),
        ("rate_limit_error", 0, "provider", "rateLimit"),
        ("billing", 0, "provider", "billing"),
        ("context_overflow", 0, "prompt", "contextOverflow"),
        ("payload_too_large", 0, "prompt", "payloadTooLarge"),
        ("max_output_reached", 0, "settings", "maxOutput"),
        ("image_too_large", 0, "prompt", "imageTooLarge"),
        ("server_error", 0, "provider", "providerRetry"),
        ("other", 0, "settings", "settingsDefault"),
    ] {
        let a = b(&format!(
            r#"{{"error":{{"streamInterruption":{{"phase":"text"}},"code":"{code}","status":{status},"provider":"p","model":"m"}}}}"#
        ));
        let p = exhausted(&a, &msg(), &[]);
        let s = p.terminal_status(&p.terminal_error().unwrap()).unwrap();
        let d = s.value().get("detail").unwrap();
        assert_eq!(d.get("fixTarget"), Some(&JsValue::from(target)));
        assert_eq!(
            d.get("userHintI18n").unwrap().get("key"),
            Some(&JsValue::from(
                format!("chat:agentStatus.modelRequestFailed.actions.{key}").as_str()
            ))
        );
    }
}
#[test]
fn status_preserves_own_null_and_nested_undefined_params() {
    let a = b(
        r#"{"error":{"streamInterruption":{"phase":"text"},"code":"model_not_found","provider":null,"protocol":null,"retryable":null}}"#,
    );
    let p = exhausted(&a, &msg(), &[]);
    let s = p.terminal_status(&p.terminal_error().unwrap()).unwrap();
    let d = s.value().get("detail").unwrap();
    assert_eq!(d.get("provider"), Some(&JsValue::Null));
    let params = d.get("userHintI18n").unwrap().get("params").unwrap();
    assert_eq!(params.get("model"), Some(&JsValue::Undefined));
    assert_eq!(params.get("provider"), Some(&JsValue::from("the provider")));
}
#[test]
fn stream_idle_timeout_source_specific_action_is_selected() {
    let a = b(
        r#"{"error":{"streamInterruption":{"phase":"text"},"code":"timeout","settingsFix":{"configPath":"model.providers.<id>.retry.streamIdleTimeoutMs"}}}"#,
    );
    let p = exhausted(&a, &msg(), &[]);
    let s = p.terminal_status(&p.terminal_error().unwrap()).unwrap();
    assert!(
        s.value()
            .get("text")
            .unwrap()
            .as_string()
            .unwrap()
            .to_utf8()
            .unwrap()
            .contains("Increase streamIdleTimeoutMs")
    );
}
#[test]
fn later_method_misuse_is_target_precondition_error_not_source_classification() {
    let p = select(
        &mut EarlyStreamRecovery::new(),
        &b(r#"{"finishReason":"stop"}"#),
        &msg(),
        &[],
        true,
    );
    assert!(matches!(
        p.retry_prompt(),
        Err(PortError::InvalidResponse { .. })
    ));
    assert!(matches!(
        p.terminal_error(),
        Err(PortError::InvalidResponse { .. })
    ));
}
