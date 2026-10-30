use super::*;
fn raw(input: &str) -> BoundarySnapshot {
    BoundarySnapshot::capture(&pilotdeck_protocol::parse_json(input).unwrap())
}
#[test]
fn ordinary_error_retains_raw_details_message_utf16_and_truthy_hint() {
    let error = raw(
        r#"{"code":"network_error","message":"partial\ud800","retryable":true,"userHint":7,"unknown":null}"#,
    );
    let (reason, classified) = classify_ordinary_model_error(&error).unwrap();
    assert!(matches!(reason, TurnStopReason::ModelError));
    assert_eq!(classified.value().get("details"), Some(error.value()));
    assert_eq!(
        classified.value().get("message"),
        error.value().get("message")
    );
    assert_eq!(
        classified.value().get("userHint"),
        Some(&JsValue::Number(7.0))
    );
    assert_eq!(
        classified.value().get("code"),
        Some(&JsValue::from("agent_model_error"))
    );
}
#[test]
fn prompt_classification_preserves_nullish_default_vs_falsy_hint() {
    for (hint, has_hint) in [
        ("null", true),
        ("false", false),
        ("0", false),
        (r#""""#, false),
    ] {
        let error = raw(&format!(
            r#"{{"code":"prompt_too_long","message":"context","userHint":{hint}}}"#
        ));
        let (reason, classified) = classify_ordinary_model_error(&error).unwrap();
        assert!(matches!(reason, TurnStopReason::PromptTooLong));
        assert_eq!(classified.value().get("userHint").is_some(), has_hint);
    }
}
#[test]
fn prompt_patterns_apply_js_coercion_and_only_ascii_nonunicode_case_fold() {
    let error = raw(r#"{"message":["PROMPT IS TOO LONG"]}"#);
    assert!(matches!(
        classify_ordinary_model_error(&error).unwrap().0,
        TurnStopReason::PromptTooLong
    ));
    assert!(!ascii_case_insensitive_contains(
        &"requeſt too large".into(),
        "request too large"
    ));
    assert!(ascii_case_insensitive_contains(
        &"REQUEST TOO LARGE\n".into(),
        "request too large"
    ));
    assert_eq!(
        model_error_regexp_text(raw(r#"[null,["a",null],false]"#).value()).unwrap(),
        JsString::from(",a,,false")
    );
}
#[test]
fn reasoning_retry_gate_uses_exact_words_case_and_source_whitespace() {
    for (message, expected) in [
        ("reasoning_content THINKING\u{a0}MODE passed back", true),
        ("xreasoning_content thinking mode passed back", false),
        ("reasoning_content thinking mode pass back", true),
        ("reasoning_content thinkingmode passed back", false),
    ] {
        let mut raw = JsObject::new();
        raw.insert("message".into(), message.into());
        assert_eq!(
            requires_reasoning_content_retry(&BoundarySnapshot::capture(&JsValue::Object(raw)))
                .unwrap(),
            expected
        );
    }
}
#[test]
fn regex_coercion_exception_occurs_only_after_source_shortcircuits() {
    let error = raw(r#"{"message":{"toString":null}}"#);
    assert!(
        matches!(classify_ordinary_model_error(&error), Err(PortError::SourceException { message, .. }) if message.equals_str("Cannot convert object to primitive value"))
    );
    let error = raw(r#"{"code":"prompt_too_long","message":{"toString":null}}"#);
    assert!(classify_ordinary_model_error(&error).is_ok());
    let error = raw(r#"{"recoverableViaCompact":true,"message":{"toString":null}}"#);
    assert!(classify_ordinary_model_error(&error).is_ok());
}
