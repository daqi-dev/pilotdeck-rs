//! Source-derived emergency terminal checks; no host/event/persistence gate.
use compaction_terminal::{EmergencyCompactionTerminalInput, EmergencyCompactionTerminalPlan};
use pilotdeck_agent::{compaction_terminal, ports};
use pilotdeck_protocol::model::BoundarySnapshot;
use pilotdeck_protocol::{JsObject, JsString, JsValue};
use ports::PortError;
#[path = "../src/compaction_terminal_policy.rs"]
mod compaction_terminal_policy;

const EMERGENCY_HINT: &str = "The context is still too large after emergency compaction. Start a new session or reduce the request and retry.";
const SETTINGS_HINT: &str = "Check Settings → Model Provider and verify the selected provider, base URL, API key, model name, and timeoutMs. If the provider is slow or the network is unstable, increase timeoutMs or check provider status.";
fn obj(fields: impl IntoIterator<Item = (&'static str, JsValue)>) -> JsValue {
    let mut object = JsObject::new();
    for (key, value) in fields {
        object.insert(key.into(), value);
    }
    JsValue::Object(object)
}
fn arr(values: impl IntoIterator<Item = JsValue>) -> JsValue {
    JsValue::Array(values.into_iter().collect())
}
fn compact(error: JsValue) -> JsValue {
    obj([
        ("error", error),
        (
            "snapshot",
            obj([
                ("tokens", JsValue::Number(111.0)),
                ("ratio", JsValue::Number(1.11)),
            ]),
        ),
        (
            "result",
            obj([(
                "diagnostics",
                obj([
                    ("tier", "emergency".into()),
                    ("extension", JsValue::Undefined),
                ]),
            )]),
        ),
    ])
}
fn evaluate(value: &JsValue) -> EmergencyCompactionTerminalPlan {
    let snapshot = BoundarySnapshot::capture(value);
    compaction_terminal_policy::evaluate(EmergencyCompactionTerminalInput { compact: &snapshot })
        .unwrap()
}
fn keys(value: &JsValue) -> Vec<String> {
    value
        .as_object()
        .unwrap()
        .entries()
        .iter()
        .map(|(key, _)| key.to_utf8().unwrap())
        .collect()
}
fn detail(plan: &EmergencyCompactionTerminalPlan) -> &JsValue {
    plan.status_detail.as_ref().unwrap().value()
}
fn error_message(plan: &EmergencyCompactionTerminalPlan) -> &JsValue {
    plan.error.value().get("message").unwrap()
}
fn status_message(plan: &EmergencyCompactionTerminalPlan) -> &JsString {
    detail(plan).get("message").unwrap().as_string().unwrap()
}
fn expected_status(prefix: &str) -> JsString {
    format!("{prefix}\n\nAction: {SETTINGS_HINT}").into()
}
fn assert_late_error(value: JsValue) {
    let raw = compact(value.clone());
    let plan = evaluate(&raw);
    assert_eq!(error_message(&plan), &value);
    let PortError::SourceException { message, boundary } = plan.status_detail.err().unwrap() else {
        panic!("known lateSource TypeError");
    };
    assert!(message.equals_str("providerMessage.trim is not a function"));
    assert_eq!(boundary.value(), &raw);
}

#[test]
fn normal_error_exact_fields_and_distinct_emergency_vs_visible_hints() {
    let raw = compact("overflow-code".into());
    let before = raw.clone();
    let plan = evaluate(&raw);
    let error = plan.error.value();
    assert_eq!(keys(error), ["code", "message", "details", "userHint"]);
    assert_eq!(
        error.get("code"),
        Some(&"agent_context_recovery_failed".into())
    );
    assert_eq!(error.get("message"), Some(&"overflow-code".into()));
    assert_eq!(error.get("userHint"), Some(&EMERGENCY_HINT.into()));
    let details = error.get("details").unwrap();
    assert_eq!(keys(details), ["code", "snapshot", "diagnostics"]);
    assert_eq!(details.get("snapshot"), raw.get("snapshot"));
    assert_eq!(
        details.get("diagnostics"),
        raw.get("result").unwrap().get("diagnostics")
    );
    assert_eq!(status_message(&plan), &expected_status("overflow-code"));
    assert_eq!(detail(&plan).get("userHint"), Some(&SETTINGS_HINT.into()));
    assert_eq!(raw, before);
}
#[test]
fn missing_and_null_error_fallback_preserve_distinct_raw_details_code() {
    for code in [JsValue::Undefined, JsValue::Null] {
        let plan = evaluate(&compact(code.clone()));
        assert_eq!(
            error_message(&plan),
            &"context_overflow_after_emergency_compaction".into()
        );
        assert_eq!(
            plan.error.value().get("details").unwrap().get("code"),
            Some(&code)
        );
        assert_eq!(
            status_message(&plan),
            &expected_status("context_overflow_after_emergency_compaction")
        );
    }
    let plan = evaluate(&obj([]));
    let details = plan.error.value().get("details").unwrap();
    for key in ["code", "snapshot", "diagnostics"] {
        assert!(details.get(key).unwrap().is_undefined());
    }
}
#[test]
fn falsy_error_values_are_retained_in_error_but_status_uses_generic_message() {
    for code in [
        JsValue::Bool(false),
        JsValue::Number(0.0),
        JsValue::Number(-0.0),
        "".into(),
    ] {
        let plan = evaluate(&compact(code.clone()));
        assert_eq!(error_message(&plan), &code);
        assert_eq!(
            status_message(&plan),
            &expected_status("The model request failed, so this turn has stopped.")
        );
        assert_eq!(
            detail(&plan).get("providerMessage"),
            Some(&"The model request failed, so this turn has stopped.".into())
        );
    }
    let plan = evaluate(&compact(JsValue::Number(f64::NAN)));
    assert!(error_message(&plan).as_number().unwrap().is_nan());
    assert_eq!(
        status_message(&plan),
        &expected_status("The model request failed, so this turn has stopped.")
    );
}
#[test]
fn truthy_numeric_boolean_array_object_are_later_trim_errors_not_outer_errors() {
    for code in [
        JsValue::Number(1.0),
        JsValue::Number(f64::INFINITY),
        JsValue::Bool(true),
        arr([]),
        obj([]),
        obj([("trim", "noncallable".into())]),
    ] {
        assert_late_error(code);
    }
}
#[test]
fn late_trim_error_keeps_fully_constructed_details_for_prior_owner_phases() {
    let raw = compact(obj([("code", "bad".into())]));
    let plan = evaluate(&raw);
    assert_eq!(
        plan.error.value().get("details").unwrap().get("code"),
        raw.get("error")
    );
    assert_eq!(
        plan.error.value().get("details").unwrap().get("snapshot"),
        raw.get("snapshot")
    );
    assert!(plan.status_detail.is_err());
    assert_eq!(
        plan.error.value().get("userHint"),
        Some(&EMERGENCY_HINT.into())
    );
}
#[test]
fn trim_is_ecmascript_whitespace_preserves_untrimmed_i18n_and_provider_message() {
    let raw = "\u{feff}\u{a0}  overflow\t\u{2028}";
    let plan = evaluate(&compact(raw.into()));
    assert_eq!(status_message(&plan), &expected_status("overflow"));
    assert_eq!(detail(&plan).get("providerMessage"), Some(&raw.into()));
    assert_eq!(
        detail(&plan)
            .get("messageI18n")
            .unwrap()
            .get("params")
            .unwrap()
            .get("providerMessage"),
        Some(&raw.into())
    );
    assert_eq!(error_message(&plan), &raw.into());
}
#[test]
fn whitespace_only_uses_trim_fallback_while_empty_string_uses_truthy_fallback() {
    let plan = evaluate(&compact(" \t\u{feff}".into()));
    assert_eq!(
        status_message(&plan),
        &expected_status("The model request failed.")
    );
    assert_eq!(
        detail(&plan).get("providerMessage"),
        Some(&" \t\u{feff}".into())
    );
    let plan = evaluate(&compact("".into()));
    assert_eq!(
        status_message(&plan),
        &expected_status("The model request failed, so this turn has stopped.")
    );
}
#[test]
fn non_js_whitespace_0085_and_zero_width_200b_are_not_trimmed() {
    for text in ["\u{85}x\u{85}", "\u{200b}x\u{200b}"] {
        let plan = evaluate(&compact(text.into()));
        assert_eq!(status_message(&plan), &expected_status(text));
    }
}
#[test]
fn wtf16_message_and_snapshot_diagnostics_are_preserved_without_lossy_conversion() {
    let message = JsString::from_utf16(vec![0xfeff, 0xd800, 0x20, 0xdfff, 0xa0]);
    let raw = compact(JsValue::String(message.clone()));
    let plan = evaluate(&raw);
    assert_eq!(error_message(&plan), &JsValue::String(message.clone()));
    let mut expected = vec![0xd800, 0x20, 0xdfff];
    expected.extend("\n\nAction: ".encode_utf16());
    expected.extend(SETTINGS_HINT.encode_utf16());
    assert_eq!(status_message(&plan).units(), &expected);
    assert_eq!(
        detail(&plan)
            .get("messageI18n")
            .unwrap()
            .get("params")
            .unwrap()
            .get("providerMessage"),
        Some(&JsValue::String(message))
    );
}
#[test]
fn status_top_level_undefined_is_pruned_but_raw_error_diagnostics_is_owned() {
    let plan = evaluate(&obj([("error", "x".into())]));
    let status = detail(&plan);
    assert_eq!(
        keys(status),
        [
            "message",
            "messageI18n",
            "code",
            "severity",
            "visible",
            "userHint",
            "userHintI18n",
            "scope",
            "source",
            "providerMessage",
            "fixTarget"
        ]
    );
    for key in [
        "provider",
        "protocol",
        "status",
        "modelErrorCode",
        "retryable",
        "settingsFix",
        "detail",
    ] {
        assert!(status.get(key).is_none(), "{key}");
    }
    assert!(
        plan.error
            .value()
            .get("details")
            .unwrap()
            .get("diagnostics")
            .unwrap()
            .is_undefined()
    );
    assert!(
        plan.error
            .value()
            .get("details")
            .unwrap()
            .get("snapshot")
            .unwrap()
            .is_undefined()
    );
    assert_eq!(status.get("severity"), Some(&"error".into()));
    assert_eq!(status.get("visible"), Some(&JsValue::Bool(true)));
    assert_eq!(status.get("scope"), Some(&"turn".into()));
    assert_eq!(status.get("source"), Some(&"agent".into()));
}
#[test]
fn emergency_error_never_impersonates_canonical_model_error_action_branch() {
    let plan = evaluate(&compact(
        "context_overflow_after_emergency_compaction".into(),
    ));
    let status = detail(&plan);
    assert_eq!(status.get("fixTarget"), Some(&"settings".into()));
    let hint = status.get("userHintI18n").unwrap();
    assert_eq!(keys(hint), ["key", "params"]);
    assert_eq!(
        hint.get("key"),
        Some(&"chat:agentStatus.modelRequestFailed.actions.settingsDefault".into())
    );
    assert!(hint.get("params").unwrap().as_object().unwrap().is_empty());
    assert_eq!(
        status.get("messageI18n").unwrap().get("key"),
        Some(&"chat:agentStatus.modelRequestFailed.message".into())
    );
}
#[test]
fn optional_result_diagnostics_handles_nullish_and_primitive_values_without_throw() {
    for result in [
        JsValue::Null,
        JsValue::Undefined,
        JsValue::Bool(false),
        JsValue::Number(0.0),
        "text".into(),
        arr([]),
    ] {
        let raw = obj([
            ("error", "x".into()),
            ("snapshot", JsValue::Null),
            ("result", result),
        ]);
        let plan = evaluate(&raw);
        let details = plan.error.value().get("details").unwrap();
        assert_eq!(details.get("snapshot"), Some(&JsValue::Null));
        assert!(details.get("diagnostics").unwrap().is_undefined());
        assert!(plan.status_detail.is_ok());
    }
    let raw = obj([
        ("error", "x".into()),
        ("snapshot", JsValue::Bool(false)),
        ("result", obj([("diagnostics", JsValue::Null)])),
    ]);
    let plan = evaluate(&raw);
    assert_eq!(
        plan.error
            .value()
            .get("details")
            .unwrap()
            .get("diagnostics"),
        Some(&JsValue::Null)
    );
}
#[test]
fn nested_diagnostics_extensions_own_undefined_numeric_keys_and_proto_are_unchanged() {
    let mut diagnostics = JsObject::new();
    diagnostics.insert("10".into(), JsValue::Number(10.0));
    diagnostics.insert("2".into(), JsValue::Number(2.0));
    diagnostics.insert("__proto__".into(), JsValue::Undefined);
    diagnostics.insert(JsString::from_utf16(vec![0xd800]), JsValue::Null);
    let diagnostics = JsValue::Object(diagnostics);
    let raw = obj([
        ("error", "x".into()),
        ("result", obj([("diagnostics", diagnostics.clone())])),
    ]);
    let plan = evaluate(&raw);
    assert_eq!(
        plan.error
            .value()
            .get("details")
            .unwrap()
            .get("diagnostics"),
        Some(&diagnostics)
    );
}
#[test]
fn nullish_compact_is_early_error_before_error_or_status_plan_exists() {
    for compact in [JsValue::Null, JsValue::Undefined] {
        let snapshot = BoundarySnapshot::capture(&compact);
        let error = compaction_terminal_policy::evaluate(EmergencyCompactionTerminalInput {
            compact: &snapshot,
        })
        .err()
        .unwrap();
        let PortError::SourceException { message, boundary } = error else {
            panic!("early Source TypeError");
        };
        assert!(message.equals_str(if matches!(compact, JsValue::Null) {
            "Cannot read properties of null (reading 'error')"
        } else {
            "Cannot read properties of undefined (reading 'error')"
        }));
        assert_eq!(boundary.value(), &compact);
    }
}
