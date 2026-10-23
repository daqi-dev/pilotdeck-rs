use super::*;
fn assert_bytes(actual: &JsValue, expected: &JsValue) {
    assert_eq!(
        pilotdeck_protocol::stringify_json(actual)
            .unwrap()
            .unwrap()
            .as_bytes(),
        pilotdeck_protocol::stringify_json(expected)
            .unwrap()
            .unwrap()
            .as_bytes(),
        "failure guard JSON bytes"
    );
}
const ORACLE: &str = include_str!("../../../tests/fixtures/failure-guard/oracle.json");
fn array(value: &JsValue) -> &[JsValue] {
    let JsValue::Array(items) = value else {
        panic!("expected array")
    };
    items
}
#[test]
fn real_ts_failure_guard_and_classify() {
    let oracle = crate::test_baseline::oracle(ORACLE);
    for case in array(field(&oracle, "cases")) {
        let mut guard = FailureGuard::new(field(case, "config").clone());
        for (index, (action, expected)) in array(field(case, "actions"))
            .iter()
            .zip(array(field(case, "results")))
            .enumerate()
        {
            let trip = if is(action, "op", "reset") {
                guard = FailureGuard::new(field(case, "config").clone());
                None
            } else if is(action, "op", "model") {
                guard.on_model_failure()
            } else if is(action, "op", "tool") {
                guard.on_tool_finished(
                    field(action, "name").as_string().unwrap(),
                    field(action, "ok") == &JsValue::Bool(true),
                    field(action, "code").as_string(),
                )
            } else {
                guard.observe(field(action, "event"))
            };
            assert_bytes(
                &trip
                    .as_ref()
                    .map(GuardTrip::as_value)
                    .unwrap_or(JsValue::Null),
                field(expected, "trip"),
            );
            assert_eq!(
                trip.as_ref()
                    .map(GuardTrip::as_value)
                    .unwrap_or(JsValue::Null),
                *field(expected, "trip"),
                "failure guard trip {:?} action {index}",
                field(case, "name")
            );
            assert_eq!(
                JsValue::Bool(guard.is_tripped()),
                *field(expected, "isTripped"),
                "failure guard latch"
            );
            if let Some(trip) = trip {
                let models = [
                    None,
                    Some(object([])),
                    Some(object([("provider", "p".into()), ("model", "m".into())])),
                    Some(object([("provider", "".into()), ("model", "".into())])),
                    Some(object([("provider", "p".into())])),
                    Some(object([(
                        "model",
                        JsValue::String(JsString::from_utf16(vec![0xd800])),
                    )])),
                ];
                for (model, message) in models.iter().zip(array(field(expected, "messages"))) {
                    assert_bytes(&build_guard_trip_message(&trip, model.as_ref()), message);
                    assert_eq!(
                        build_guard_trip_message(&trip, model.as_ref()),
                        *message,
                        "failure guard message"
                    );
                }
            }
        }
    }
    for case in array(field(&oracle, "classify")) {
        let name = field(case, "name").as_string().unwrap();
        let config = field(&oracle, "config");
        let resolved = resolve_tool_unit(name, field(config, "toolFailureLimits"))
            .map(|(unit, limit)| {
                object([
                    ("unit", JsValue::String(unit)),
                    ("limit", JsValue::Number(limit)),
                ])
            })
            .unwrap_or(JsValue::Null);
        assert_eq!(resolved, *field(case, "resolved"), "failure guard classify");
        assert_eq!(
            JsValue::String(tool_label(name, field(config, "toolLabels"))),
            *field(case, "label"),
            "failure guard label"
        );
    }
    for case in array(field(&oracle, "counted")) {
        assert_eq!(
            JsValue::Bool(is_counted_tool_failure(field(case, "result"))),
            *field(case, "counted"),
            "failure guard counted code"
        );
    }
}
#[test]
#[ignore = "requires real fixed TS baseline and Node 22"]
fn real_ts_failure_guard_regenerates_identically() {
    crate::test_baseline::regenerates_identically("failure-guard");
}
