use pilotdeck_gateway::hooks::{config::*, protocol::*};
use pilotdeck_protocol::{JsObject, JsValue, parse_json, stringify_json};

fn array(value: &JsValue) -> &[JsValue] {
    let JsValue::Array(items) = value else {
        panic!("expected array: {value:?}")
    };
    items
}
fn text(value: &JsValue) -> String {
    value.as_string().unwrap().to_utf8().unwrap()
}
fn decode(value: &JsValue) -> JsValue {
    let parts = array(value);
    match text(&parts[0]).as_str() {
        "undefined" => JsValue::Undefined,
        "null" => JsValue::Null,
        "boolean" | "string" => parts[1].clone(),
        "number" => match &parts[1] {
            JsValue::Number(n) => JsValue::Number(*n),
            s => JsValue::Number(match text(s).as_str() {
                "NaN" => f64::NAN,
                "Infinity" => f64::INFINITY,
                "-Infinity" => f64::NEG_INFINITY,
                "-0" => -0.0,
                other => panic!("bad number {other}"),
            }),
        },
        "array" => JsValue::Array(array(&parts[1]).iter().map(decode).collect()),
        "object" => {
            let mut out = JsObject::new();
            for entry in array(&parts[1]) {
                let pair = array(entry);
                out.insert(pair[0].as_string().unwrap().clone(), decode(&pair[1]));
            }
            if !matches!(parts[2], JsValue::Null) {
                out.set_prototype(decode(&parts[2]));
            }
            JsValue::Object(out)
        }
        other => panic!("bad tag {other}"),
    }
}
fn oracle() -> JsValue {
    parse_json(include_str!("fixtures/hooks-config/oracle.json")).unwrap()
}
fn assert_json(actual: &JsValue, case: &JsValue, field: &str) {
    assert_eq!(
        stringify_json(actual).unwrap().unwrap(),
        text(case.get(field).unwrap()),
        "{}",
        text(case.get("name").unwrap())
    );
}
#[test]
fn parses_every_ts_diagnostic_and_retains_valid_hooks() {
    let fixtures = oracle();
    let cases = array(fixtures.get("parses").unwrap());
    assert!(cases.len() >= 30);
    for case in cases {
        let input = decode(case.get("input").unwrap());
        assert_json(&parse_hooks_config(&input).to_value(), case, "expected");
    }
}
#[test]
fn matcher_matrix_matches_real_ts_including_legacy_regexp() {
    let fixtures = oracle();
    let mut failures = Vec::new();
    for case in array(fixtures.get("matchers").unwrap()) {
        let matcher = decode(case.get("matcher").unwrap());
        let query = decode(case.get("query").unwrap());
        let result = match_hook_matcher(matcher.as_string(), query.as_string());
        if result != case.get("expected").unwrap().as_bool().unwrap() {
            failures.push(format!(
                "{}: matcher={matcher:?}, query={query:?}, actual={result}",
                text(case.get("name").unwrap())
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "{} mismatches:\n{}",
        failures.len(),
        failures.join("\n")
    );
}
#[test]
fn conditions_match_real_ts_json_and_tool_name_rules() {
    let fixtures = oracle();
    for case in array(fixtures.get("conditions").unwrap()) {
        let condition = decode(case.get("condition").unwrap());
        let input = decode(case.get("input").unwrap());
        let result = match_hook_condition(
            condition.as_string(),
            HookConditionInput {
                tool_name: input.get("toolName").and_then(JsValue::as_string),
                tool_input: input.get("toolInput"),
            },
        );
        assert_eq!(
            result,
            case.get("expected").unwrap().as_bool().unwrap(),
            "{}",
            text(case.get("name").unwrap())
        );
    }
}
#[test]
fn input_creation_and_legacy_projection_match_real_ts() {
    let fixtures = oracle();
    for case in array(fixtures.get("inputs").unwrap()) {
        let event = decode(case.get("event").unwrap());
        let base = decode(case.get("base").unwrap());
        let payload = decode(case.get("payload").unwrap());
        let created = create_hook_input(
            event.as_string().unwrap(),
            base.as_object().unwrap(),
            payload.as_object().unwrap(),
        );
        let legacy = to_legacy_hook_input(&created);
        assert_json(&JsValue::Object(created), case, "expected");
        assert_json(&JsValue::Object(legacy), case, "legacy");
    }
}
#[test]
fn supported_and_legacy_events_match_real_ts() {
    let fixtures = oracle();
    for case in array(fixtures.get("events").unwrap()) {
        let value = decode(case.get("value").unwrap());
        assert_eq!(
            is_hook_event(value.as_string().unwrap()),
            case.get("expected").unwrap().as_bool().unwrap(),
            "{value:?}"
        );
    }
    assert_eq!(HOOK_EVENTS.len(), 25);
    assert!(
        NOT_APPLICABLE_LEGACY_HOOK_EVENTS
            .iter()
            .all(|name| !is_hook_event(&(*name).into()))
    );
}
