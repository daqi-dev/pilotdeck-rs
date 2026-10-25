#[path = "common/baseline.rs"]
mod baseline;
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
        "json" => parse_json(&text(&parts[1])).unwrap(),
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
    baseline::oracle(include_str!("fixtures/hooks-config/oracle.json"))
}
#[test]
fn oracle_is_bound_to_the_fixed_ts_sources() {
    let fixtures = oracle();
    assert_eq!(
        text(fixtures.get("sourceCommit").unwrap()),
        "cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85"
    );
    assert_eq!(text(fixtures.get("nodeVersion").unwrap()), "v22.22.0");
    assert_eq!(text(fixtures.get("unicodeVersion").unwrap()), "17.0");
    let sources = fixtures.get("sourceSha256").unwrap().as_object().unwrap();
    let paths = [
        "config/parseHooksConfig.ts",
        "config/matchHook.ts",
        "config/matchHookCondition.ts",
        "protocol/events.ts",
        "protocol/input.ts",
        "protocol/output.ts",
        "protocol/settings.ts",
    ];
    assert_eq!(sources.entries().len(), paths.len());
    for path in paths {
        let path = format!("src/extension/hooks/{path}");
        let expected = text(sources.get(&path).expect("missing source digest"));
        assert_eq!(expected.len(), 64, "{path}");
    }
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
    assert_eq!(cases.len(), 104);
    for case in cases {
        let input = decode(case.get("input").unwrap());
        assert_json(&parse_hooks_config(&input).to_value(), case, "expected");
    }
}
#[test]
fn matcher_matrix_matches_real_ts_including_legacy_regexp() {
    let fixtures = oracle();
    let mut failures = Vec::new();
    let cases = array(fixtures.get("matchers").unwrap());
    assert_eq!(cases.len(), 9800);
    for case in cases {
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
    let cases = array(fixtures.get("conditions").unwrap());
    assert_eq!(cases.len(), 194);
    for case in cases {
        let condition = decode(case.get("condition").unwrap());
        let input = decode(case.get("input").unwrap());
        let result = match_hook_condition(
            condition.as_string(),
            HookConditionInput {
                tool_name: input.get("toolName").and_then(JsValue::as_string),
                tool_input: input.get("toolInput"),
            },
        );
        let name = text(case.get("name").unwrap());
        if let Some(expected) = case.get("error") {
            let error = result.expect_err(&name);
            assert_eq!(error.name(), text(expected.get("name").unwrap()), "{name}");
            assert_eq!(
                error.message(),
                text(expected.get("message").unwrap()),
                "{name}"
            );
            assert_eq!(
                error.to_string(),
                format!("{}: {}", error.name(), error.message())
            );
        } else {
            assert_eq!(
                result.unwrap(),
                case.get("expected").unwrap().as_bool().unwrap(),
                "{name}"
            );
        }
    }
}

// These assertions pin the approved conservative policy inside the accepted
// uncertainty band. They are deliberately separate from the real TS oracle.
#[test]
fn conservative_resource_budgets_are_inclusive() {
    let query = "b".into();
    assert!(match_hook_matcher(
        Some(&"/^(?:a?){2000000}b$/".into()),
        Some(&query)
    ));
    assert!(!match_hook_matcher(
        Some(&"/^(?:a?){2000001}b$/".into()),
        Some(&query)
    ));
    assert!(!match_hook_matcher(
        Some(&"/^(?!(a?){1000001}c)b$/".into()),
        Some(&query)
    ));
    for (depth, expected) in [
        (5000, Ok(true)),
        (6500, Ok(true)),
        (6501, Err(HookConditionError::StackOverflow)),
    ] {
        let input = parse_json(&format!("{}0{}", "[".repeat(depth), "]".repeat(depth))).unwrap();
        let result = match_hook_condition(
            Some(&"Read(0)".into()),
            HookConditionInput {
                tool_name: None,
                tool_input: Some(&input),
            },
        );
        assert_eq!(result, expected, "depth {depth}");
    }
}
#[test]
fn input_creation_and_legacy_projection_match_real_ts() {
    let fixtures = oracle();
    let cases = array(fixtures.get("inputs").unwrap());
    assert_eq!(cases.len(), 8);
    for case in cases {
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
    let cases = array(fixtures.get("events").unwrap());
    assert_eq!(cases.len(), 32);
    for case in cases {
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

/// Regenerates the oracle from the real TS hook config code and compares bytes.
#[test]
#[ignore = "requires PDRS_BASELINE (PilotDeck cc7c4e6 with node_modules)"]
fn hooks_config_fixtures_regenerate_identically_from_ts() {
    baseline::regenerates_identically("hooks-config");
}

/// `identifier_ranges.rs` must equal what Node v22.22.0 records now.
#[test]
#[ignore = "requires Node v22.22.0 (Unicode 17.0)"]
fn identifier_ranges_are_fresh() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let output = std::process::Command::new("node")
        .arg(root.join("tests/fixtures/hooks-config/generate-identifiers.mjs"))
        .arg(root.join("src/hooks/identifier_ranges.rs"))
        .arg("--check")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
