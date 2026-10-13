//! Source-derived pure transform tests, supplementary to original host gates.
use pilotdeck_agent::{ports, request_materialization};
use pilotdeck_protocol::model::BoundarySnapshot;
use pilotdeck_protocol::{JsObject, JsString, JsValue};
use ports::{PortError, Route};
use request_materialization::{BudgetCandidateInput, PreparedWindowInput, RequestCapsInput};
#[path = "../src/request_materialization_policy.rs"]
mod request_materialization_policy;

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
fn route() -> Route {
    Route {
        provider: "prepared-provider".into(),
        model: "prepared-model".into(),
    }
}
fn budget(base: &JsValue, preview: &JsValue) -> Result<BoundarySnapshot, PortError> {
    request_materialization_policy::budget_candidate(BudgetCandidateInput {
        base_request: &BoundarySnapshot::capture(base),
        preview_request: &BoundarySnapshot::capture(preview),
    })
}
fn window(prepared: &JsValue, candidate: &JsValue, route: &Route) -> BoundarySnapshot {
    request_materialization_policy::default_prepared_window(PreparedWindowInput {
        prepared_request: &BoundarySnapshot::capture(prepared),
        candidate_request: &BoundarySnapshot::capture(candidate),
        route,
    })
    .unwrap()
}
fn caps(
    request: &JsValue,
    current: Option<f64>,
    attempt: bool,
) -> Result<BoundarySnapshot, PortError> {
    request_materialization_policy::apply_caps(RequestCapsInput {
        request: &BoundarySnapshot::capture(request),
        route: &route(),
        current_max_output_tokens: current,
        attempt_override_present: attempt,
    })
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
fn fixture() -> JsValue {
    obj([
        ("model", "base-model".into()),
        ("systemPrompt", "prepared prompt".into()),
        ("messages", arr(["old".into()])),
        ("provider", "base-provider".into()),
        ("tools", arr(["prepared-tool".into()])),
        ("maxOutputTokens", JsValue::Number(128.75)),
        ("cachePlan", obj([("keep", JsValue::Bool(true))])),
        ("extension", obj([("x", JsValue::Undefined)])),
    ])
}
fn preview() -> JsValue {
    obj([
        ("messages", arr(["new".into()])),
        ("systemPrompt", "preview prompt".into()),
        ("tools", arr(["preview-tool".into()])),
        ("cacheBreakpoints", arr([JsValue::Number(2.0)])),
        ("cachePlan", JsValue::Null),
        ("maxOutputTokens", JsValue::Number(999.0)),
        ("extension", "ignore".into()),
    ])
}

#[test]
fn budget_merges_exactly_five_preview_fields_preserving_controls_and_slots() {
    let base = fixture();
    let preview = preview();
    let before = base.clone();
    let result = budget(&base, &preview).unwrap();
    let value = result.value();
    for key in [
        "messages",
        "systemPrompt",
        "tools",
        "cacheBreakpoints",
        "cachePlan",
    ] {
        assert_eq!(value.get(key), preview.get(key));
    }
    for key in ["provider", "model", "maxOutputTokens", "extension"] {
        assert_eq!(value.get(key), base.get(key));
    }
    assert_eq!(
        keys(value),
        [
            "model",
            "systemPrompt",
            "messages",
            "provider",
            "tools",
            "maxOutputTokens",
            "cachePlan",
            "extension",
            "cacheBreakpoints"
        ]
    );
    assert_eq!(base, before);
}
#[test]
fn absent_preview_fields_become_own_undefined_not_stale_base_values() {
    let result = budget(&fixture(), &obj([])).unwrap();
    for key in [
        "messages",
        "systemPrompt",
        "tools",
        "cacheBreakpoints",
        "cachePlan",
    ] {
        assert!(result.value().get(key).unwrap().is_undefined(), "{key}");
    }
    let json = pilotdeck_protocol::stringify_json(result.value())
        .unwrap()
        .unwrap();
    assert!(!json.contains("systemPrompt"));
    assert!(!json.contains("messages"));
    assert!(json.contains("maxOutputTokens"));
}
#[test]
fn budget_nullish_preview_errors_before_any_result_not_empty_normalization() {
    for (preview, kind) in [(JsValue::Null, "null"), (JsValue::Undefined, "undefined")] {
        let error = budget(&fixture(), &preview).unwrap_err();
        let PortError::SourceException { message, boundary } = error else {
            panic!("source TypeError");
        };
        assert!(message.equals_str(&format!(
            "Cannot read properties of {kind} (reading 'messages')"
        )));
        assert_eq!(boundary.value(), &preview);
    }
}
#[test]
fn budget_base_primitive_spread_is_not_a_schema_rejection() {
    let result = budget(&JsValue::Null, &preview()).unwrap();
    assert_eq!(
        keys(result.value()),
        [
            "messages",
            "systemPrompt",
            "tools",
            "cacheBreakpoints",
            "cachePlan"
        ]
    );
    let wtf = JsString::from_utf16(vec![0xd800, 0xdc00, 0xdfff]);
    let result = budget(&JsValue::String(wtf), &obj([])).unwrap();
    assert_eq!(
        result.value().get("2"),
        Some(&JsValue::String(JsString::from_utf16(vec![0xdfff])))
    );
    let result = budget(
        &arr([JsValue::Undefined, JsValue::Null]),
        &JsValue::Number(7.0),
    )
    .unwrap();
    assert!(result.value().get("0").unwrap().is_undefined());
    assert!(result.value().get("messages").unwrap().is_undefined());
}
#[test]
fn prepared_window_retains_prompt_tools_output_controls_and_extensions() {
    let prepared = fixture();
    let candidate = preview();
    let before = prepared.clone();
    let result = window(&prepared, &candidate, &route());
    for key in ["messages", "cacheBreakpoints", "cachePlan"] {
        assert_eq!(result.value().get(key), candidate.get(key));
    }
    for key in ["systemPrompt", "tools", "maxOutputTokens", "extension"] {
        assert_eq!(result.value().get(key), prepared.get(key));
    }
    assert_eq!(
        result.value().get("provider"),
        Some(&JsValue::String(route().provider))
    );
    assert_eq!(
        result.value().get("model"),
        Some(&JsValue::String(route().model))
    );
    assert_eq!(prepared, before);
    assert_eq!(
        keys(result.value()),
        [
            "model",
            "systemPrompt",
            "messages",
            "provider",
            "tools",
            "maxOutputTokens",
            "cachePlan",
            "extension",
            "cacheBreakpoints"
        ]
    );
}
#[test]
fn prepared_window_nullish_candidate_is_routed_object_before_field_reads() {
    for candidate in [
        JsValue::Null,
        JsValue::Undefined,
        JsValue::Bool(false),
        JsValue::Number(7.0),
    ] {
        let result = window(&fixture(), &candidate, &route());
        for key in ["messages", "cacheBreakpoints", "cachePlan"] {
            assert!(result.value().get(key).unwrap().is_undefined());
        }
        assert_eq!(
            result.value().get("systemPrompt"),
            fixture().get("systemPrompt")
        );
    }
    let result = window(&JsValue::Null, &JsValue::Null, &route());
    assert_eq!(
        keys(result.value()),
        [
            "messages",
            "cacheBreakpoints",
            "cachePlan",
            "provider",
            "model"
        ]
    );
}
#[test]
fn prepared_window_explicit_null_caches_remain_null() {
    let candidate = obj([
        ("messages", JsValue::Null),
        ("cacheBreakpoints", JsValue::Null),
        ("cachePlan", JsValue::Null),
    ]);
    let result = window(&fixture(), &candidate, &route());
    for key in ["messages", "cacheBreakpoints", "cachePlan"] {
        assert_eq!(result.value().get(key), Some(&JsValue::Null));
    }
}
#[test]
fn prepared_finite_positive_cap_is_floored_and_never_enlarged_without_override() {
    for (prepared, current, expected) in [
        (128.75, 1000.0, 128.0),
        (128.75, 64.0, 64.0),
        (0.25, 64.0, 0.0),
        (999.9, 0.0, 0.0),
    ] {
        let request = obj([("maxOutputTokens", JsValue::Number(prepared))]);
        let result = caps(&request, Some(current), false).unwrap();
        assert_eq!(
            result.value().get("maxOutputTokens"),
            Some(&JsValue::Number(expected))
        );
    }
}
#[test]
fn attempt_override_presence_enlarges_prepared_smaller_cap() {
    let request = obj([("maxOutputTokens", JsValue::Number(8.0))]);
    let result = caps(&request, Some(100.0), true).unwrap();
    assert_eq!(
        result.value().get("maxOutputTokens"),
        Some(&JsValue::Number(100.0))
    );
    let result = caps(&request, None, true).unwrap();
    assert_eq!(
        result.value().get("maxOutputTokens"),
        request.get("maxOutputTokens")
    );
}
#[test]
fn missing_current_cap_does_not_floor_or_validate_prepared_raw_values() {
    for value in [
        JsValue::Number(8.75),
        JsValue::Number(-1.0),
        JsValue::Number(f64::INFINITY),
        JsValue::Null,
        JsValue::Bool(false),
        "open".into(),
        obj([]),
    ] {
        let request = obj([("maxOutputTokens", value.clone())]);
        let result = caps(&request, None, false).unwrap();
        assert_eq!(result.value().get("maxOutputTokens"), Some(&value));
    }
    let request = obj([("maxOutputTokens", JsValue::Number(f64::NAN))]);
    let result = caps(&request, None, false).unwrap();
    assert!(
        result
            .value()
            .get("maxOutputTokens")
            .unwrap()
            .as_number()
            .unwrap()
            .is_nan()
    );
}
#[test]
fn current_cap_replaces_lax_nonpositive_nonfinite_prepared_values() {
    for value in [
        JsValue::Null,
        JsValue::Bool(false),
        "8".into(),
        JsValue::Number(-1.0),
        JsValue::Number(0.0),
        JsValue::Number(f64::INFINITY),
        JsValue::Number(f64::NAN),
        obj([]),
    ] {
        let request = obj([("maxOutputTokens", value)]);
        let result = caps(&request, Some(64.0), false).unwrap();
        assert_eq!(
            result.value().get("maxOutputTokens"),
            Some(&JsValue::Number(64.0))
        );
    }
}
#[test]
fn missing_vs_own_undefined_cap_and_original_insertion_slots_survive() {
    let absent = caps(&obj([]), None, false).unwrap();
    assert!(absent.value().get("maxOutputTokens").is_none());
    assert_eq!(keys(absent.value()), ["provider", "model"]);
    let own = caps(
        &obj([
            ("maxOutputTokens", JsValue::Undefined),
            ("model", "old".into()),
            ("extension", JsValue::Null),
        ]),
        None,
        false,
    )
    .unwrap();
    assert!(own.value().get("maxOutputTokens").unwrap().is_undefined());
    assert_eq!(
        keys(own.value()),
        ["maxOutputTokens", "model", "extension", "provider"]
    );
    assert_eq!(
        pilotdeck_protocol::stringify_json(own.value())
            .unwrap()
            .unwrap(),
        "{\"model\":\"prepared-model\",\"extension\":null,\"provider\":\"prepared-provider\"}"
    );
}
#[test]
fn apply_caps_nullish_request_fails_before_spread_or_route_replacement() {
    for request in [JsValue::Null, JsValue::Undefined] {
        let error = caps(&request, Some(10.0), true).unwrap_err();
        let PortError::SourceException { message, boundary } = error else {
            panic!("source TypeError");
        };
        assert!(message.equals_str(if matches!(request, JsValue::Null) {
            "Cannot read properties of null (reading 'maxOutputTokens')"
        } else {
            "Cannot read properties of undefined (reading 'maxOutputTokens')"
        }));
        assert_eq!(boundary.value(), &request);
    }
}
#[test]
fn integer_key_order_own_proto_and_unpaired_utf16_keys_remain_lossless() {
    let mut request = JsObject::new();
    request.insert("x".into(), JsValue::Null);
    request.insert("10".into(), JsValue::Number(10.0));
    request.insert("2".into(), JsValue::Number(2.0));
    request.insert("01".into(), JsValue::Number(1.0));
    request.insert("__proto__".into(), obj([("value", JsValue::Undefined)]));
    let key = JsString::from_utf16(vec![0xd800]);
    request.insert(key.clone(), "retained".into());
    let result = caps(&JsValue::Object(request), None, false).unwrap();
    let entries = result.value().as_object().unwrap().entries();
    assert!(entries[0].0.equals_str("2"));
    assert!(entries[1].0.equals_str("10"));
    assert_eq!(
        result.value().as_object().unwrap().get_key(&key),
        Some(&"retained".into())
    );
    assert!(result.value().get("__proto__").is_some());
}
#[test]
fn route_labels_preserve_wtf16_and_do_not_coerce_to_replacement_utf8() {
    let route = Route {
        provider: JsString::from_utf16(vec![0xd800]),
        model: JsString::from_utf16(vec![0xdfff]),
    };
    let result = window(&fixture(), &preview(), &route);
    assert_eq!(
        result.value().get("provider"),
        Some(&JsValue::String(route.provider))
    );
    assert_eq!(
        result.value().get("model"),
        Some(&JsValue::String(route.model))
    );
}
#[test]
fn raw_numeric_apply_preserves_math_min_nan_and_signed_zero_semantics() {
    // currentMaxOutputTokens owner normally emits finite nonnegative floors.
    // These direct pure-input controls are robustness cases, not claims that
    // Source's normal owner emits NaN or negative zero.
    let request = obj([("maxOutputTokens", JsValue::Number(0.25))]);
    let result = caps(&request, Some(-0.0), false).unwrap();
    assert_eq!(
        result
            .value()
            .get("maxOutputTokens")
            .unwrap()
            .as_number()
            .unwrap()
            .to_bits(),
        (-0.0f64).to_bits()
    );
    let result = caps(&request, Some(f64::NAN), false).unwrap();
    assert!(
        result
            .value()
            .get("maxOutputTokens")
            .unwrap()
            .as_number()
            .unwrap()
            .is_nan()
    );
}
