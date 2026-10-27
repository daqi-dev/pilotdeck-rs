use super::*;
fn assert_bytes(actual: &JsValue, expected: &JsValue, message: &str) {
    assert_eq!(
        pilotdeck_protocol::stringify_json(actual)
            .unwrap()
            .unwrap()
            .as_bytes(),
        pilotdeck_protocol::stringify_json(expected)
            .unwrap()
            .unwrap()
            .as_bytes(),
        "{message}"
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
        let case_name = field(case, "name").as_string().unwrap().to_utf8().unwrap();
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
                &format!(
                    "#813 failure guard trip: {case_name} action {index}: trip result bytes must match"
                ),
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
                "#813 failure guard latch: trip latch must stay set until reset"
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
                    assert_bytes(
                        &build_guard_trip_message(&trip, model.as_ref()),
                        message,
                        &format!(
                            "#813 failure guard message: {case_name} action {index}: trip message bytes must match"
                        ),
                    );
                    assert_eq!(
                        build_guard_trip_message(&trip, model.as_ref()),
                        *message,
                        "#813 failure guard message: trip message value must match"
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
        assert_eq!(
            resolved,
            *field(case, "resolved"),
            "#813 failure guard classify: resolved unit and limit must match"
        );
        assert_eq!(
            JsValue::String(tool_label(name, field(config, "toolLabels"))),
            *field(case, "label"),
            "#813 failure guard label: tool label must match"
        );
    }
    for case in array(field(&oracle, "counted")) {
        assert_eq!(
            JsValue::Bool(is_counted_tool_failure(field(case, "result"))),
            *field(case, "counted"),
            "#813 failure guard counted: counted failure code must match"
        );
    }
}
#[test]
#[ignore = "requires real fixed TS baseline and Node 22"]
fn real_ts_failure_guard_regenerates_identically() {
    crate::test_baseline::regenerates_identically("failure-guard");
}
