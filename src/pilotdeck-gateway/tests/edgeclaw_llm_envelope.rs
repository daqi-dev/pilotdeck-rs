//! Real TS oracle for A6.6-edgeclaw-7 JSON envelopes and normalizers.
use pilotdeck_gateway::runtime::memory::edgeclaw::llm::envelope::{
    LooseMemoryCreatePayload, extract_first_json_object, extract_loose_json_boolean_property,
    extract_loose_json_envelope, extract_loose_json_string_property, normalize_boolean,
    normalize_memory_route, normalize_string_array, normalize_whitespace, truncate,
    truncate_for_prompt, try_parse_loose_memory_create_payload, unique_strings,
};
use pilotdeck_protocol::{JsObject, JsString, JsValue, parse_json, stringify_json};

#[path = "common/baseline.rs"]
mod baseline;
const ORACLE: &str = include_str!("fixtures/edgeclaw-llm-envelope/oracle.json");

fn oracle() -> JsValue {
    baseline::oracle(ORACLE);
    parse_json(ORACLE).unwrap()
}

fn decode(value: &JsValue) -> JsValue {
    match value {
        JsValue::Array(items) => JsValue::Array(items.iter().map(decode).collect()),
        JsValue::Object(object) => {
            if object.len() == 1 && matches!(object.get("$undefined"), Some(JsValue::Bool(true))) {
                return JsValue::Undefined;
            }
            if object.len() == 1
                && let Some(JsValue::String(kind)) = object.get("$number")
                && let Ok(kind) = kind.to_utf8()
            {
                return JsValue::Number(match kind.as_str() {
                    "NaN" => f64::NAN,
                    "Infinity" => f64::INFINITY,
                    "-Infinity" => f64::NEG_INFINITY,
                    "-0" => -0.0,
                    other => other.parse().expect("number sentinel"),
                });
            }
            let mut decoded = JsObject::new();
            for (key, item) in object.entries() {
                decoded.insert(key.clone(), decode(item));
            }
            JsValue::Object(decoded)
        }
        other => other.clone(),
    }
}

fn wire(value: &JsValue) -> String {
    match stringify_json(value).unwrap() {
        Some(text) => text,
        None => "undefined".into(),
    }
}

fn group(name: &str) -> &'static str {
    match name {
        "extractFirstJsonObject" => "object",
        "extractLooseJsonEnvelope" => "envelope",
        "extractLooseJsonBooleanProperty" => "loose-bool",
        "extractLooseJsonStringProperty" => "loose-string",
        "tryParseLooseMemoryCreatePayload" => "payload",
        "normalizeStringArray" | "uniqueStrings" => "arrays",
        "truncate" | "normalizeWhitespace" | "truncateForPrompt" => "truncate",
        "normalizeMemoryRoute" | "normalizeBoolean" => "route",
        other => panic!("oracle has an unknown function {other}"),
    }
}

fn items(value: &JsValue) -> &[JsValue] {
    match value {
        JsValue::Array(items) => items,
        other => panic!("expected an array, got {other:?}"),
    }
}

fn args(case: &JsValue) -> &[JsValue] {
    items(case.get("args").unwrap())
}

fn text(value: &JsValue) -> &JsString {
    value.as_string().expect("string argument")
}

fn number(case: &JsValue, index: usize) -> f64 {
    args(case)[index].as_number().expect("number argument")
}

fn payload_value(payload: LooseMemoryCreatePayload) -> JsValue {
    // TS spreads skip, reason, name, description, markdown in that order.
    let mut object = JsObject::new();
    if let Some(skip) = payload.skip {
        object.insert("skip".into(), JsValue::Bool(skip));
    }
    for (key, value) in [
        ("reason", payload.reason),
        ("name", payload.name),
        ("description", payload.description),
        ("markdown", payload.markdown),
    ] {
        if let Some(value) = value {
            object.insert(key.into(), JsValue::String(value));
        }
    }
    JsValue::Object(object)
}

fn call(case: &JsValue) -> Result<JsValue, String> {
    let name = text(case.get("fn").unwrap()).to_utf8().unwrap();
    let args = args(case);
    match name.as_str() {
        "extractFirstJsonObject" => extract_first_json_object(text(&args[0]))
            .map(JsValue::String)
            .map_err(|error| error.to_string()),
        "extractLooseJsonEnvelope" => extract_loose_json_envelope(text(&args[0]))
            .map(JsValue::String)
            .map_err(|error| error.to_string()),
        "extractLooseJsonBooleanProperty" => Ok(
            match extract_loose_json_boolean_property(text(&args[0]), text(&args[1])) {
                Some(value) => JsValue::Bool(value),
                None => JsValue::Undefined,
            },
        ),
        "extractLooseJsonStringProperty" => {
            let next = items(&args[2])
                .iter()
                .map(|item| text(item).clone())
                .collect::<Vec<_>>();
            Ok(
                match extract_loose_json_string_property(text(&args[0]), text(&args[1]), &next) {
                    Some(value) => JsValue::String(value),
                    None => JsValue::Undefined,
                },
            )
        }
        "tryParseLooseMemoryCreatePayload" => try_parse_loose_memory_create_payload(text(&args[0]))
            .map(|payload| match payload {
                Some(payload) => payload_value(payload),
                None => JsValue::Null,
            })
            .map_err(|error| error.to_string()),
        "normalizeStringArray" => Ok(JsValue::Array(
            normalize_string_array(&args[0], number(case, 1))
                .into_iter()
                .map(JsValue::String)
                .collect(),
        )),
        "uniqueStrings" => {
            let values = items(&args[0])
                .iter()
                .map(|item| text(item).clone())
                .collect::<Vec<_>>();
            Ok(JsValue::Array(
                unique_strings(&values, number(case, 1))
                    .into_iter()
                    .map(JsValue::String)
                    .collect(),
            ))
        }
        "truncate" => Ok(JsValue::String(truncate(text(&args[0]), number(case, 1)))),
        "normalizeWhitespace" => Ok(JsValue::String(normalize_whitespace(text(&args[0])))),
        "truncateForPrompt" => Ok(JsValue::String(truncate_for_prompt(
            text(&args[0]),
            number(case, 1),
        ))),
        "normalizeMemoryRoute" => Ok(JsValue::String(JsString::from(
            normalize_memory_route(&args[0]).as_str(),
        ))),
        "normalizeBoolean" => Ok(JsValue::Bool(normalize_boolean(
            &args[0],
            args[1].as_bool().expect("boolean fallback"),
        ))),
        other => panic!("oracle has an unknown function {other}"),
    }
}

fn replay(wanted: &str) {
    let oracle = oracle();
    let clock_ms = oracle.get("clockMs").unwrap().as_number().unwrap();
    let mut seen = 0;
    for case in items(oracle.get("cases").unwrap()) {
        let name = text(case.get("fn").unwrap()).to_utf8().unwrap();
        if group(&name) != wanted {
            continue;
        }
        seen += 1;
        let decoded = decode(case);
        let actual = call(&decoded);
        match (actual, decoded.get("value"), decoded.get("threw")) {
            (Ok(actual), Some(expected), None) => assert_eq!(
                wire(&actual),
                wire(expected),
                "{name} args={:?} clock={clock_ms}",
                decoded.get("args").unwrap()
            ),
            (Err(actual), None, Some(expected)) => assert_eq!(
                actual,
                expected.as_string().unwrap().to_utf8().unwrap(),
                "{name} args={:?}",
                decoded.get("args").unwrap()
            ),
            (actual, expected, threw) => {
                panic!("{name}: unexpected oracle shape {actual:?} {expected:?} {threw:?}")
            }
        }
    }
    assert!(seen > 0, "no {wanted} cases in the oracle");
}

#[test]
fn json_object_matches_ts() {
    replay("object");
}

#[test]
fn json_envelope_matches_ts() {
    replay("envelope");
}

#[test]
fn loose_boolean_matches_ts() {
    replay("loose-bool");
}

#[test]
fn loose_string_matches_ts() {
    replay("loose-string");
}

#[test]
fn loose_payload_matches_ts() {
    replay("payload");
}

#[test]
fn string_arrays_match_ts() {
    replay("arrays");
}

#[test]
fn truncation_matches_ts() {
    replay("truncate");
}

#[test]
fn route_and_boolean_match_ts() {
    replay("route");
}

#[test]
#[ignore = "requires fixed PDRS_BASELINE with Node 22 and tsx"]
fn fixture_regenerates_identically() {
    baseline::regenerates_identically("edgeclaw-llm-envelope");
}
