//! Replay of `tests/fixtures/jsonrepair/oracle.json`: the baseline's real
//! `jsonrepair` 3.14.0 and `normalizeAnthropicStreamEvent` on 86 broken
//! tool-argument inputs. Repaired text is compared unit for unit; the stream
//! events (tool_call_end with `wasRepaired`, or the deferred
//! `invalid_tool_arguments` error) byte for byte as JSON.
use super::jsonrepair;
use crate::runtime::model::providers::anthropic::{StreamState, normalize_stream_event};
use crate::test_baseline as baseline;
use pilotdeck_protocol::{JsValue, parse_json, stringify_json};

const ORACLE: &str = include_str!("../../../tests/fixtures/jsonrepair/oracle.json");

fn get<'a>(value: &'a JsValue, key: &str) -> &'a JsValue {
    static UNDEFINED: JsValue = JsValue::Undefined;
    value.get(key).unwrap_or(&UNDEFINED)
}
fn json(value: &JsValue) -> String {
    stringify_json(value).unwrap().unwrap_or_default()
}

#[test]
fn real_ts_jsonrepair_and_tool_argument_events() {
    let oracle = baseline::oracle(ORACLE);
    assert!(
        get(&oracle, "jsonrepair")
            .as_string()
            .unwrap()
            .equals_str("3.14.0")
    );
    let JsValue::Array(cases) = get(&oracle, "cases") else {
        panic!("cases")
    };
    assert_eq!(cases.len(), 86);
    let (mut repaired_count, mut failed_count) = (0, 0);
    for case in cases {
        let input = get(case, "input").as_string().unwrap();
        let label = format!("{:?}", String::from_utf16_lossy(input.units()));
        let actual = jsonrepair(input.units());
        match get(case, "repaired") {
            JsValue::String(expected) => {
                assert_eq!(
                    actual.as_deref().ok(),
                    Some(expected.units()),
                    "repaired {label}"
                );
            }
            _ => assert!(actual.is_err(), "{label}: TS throws, Rust gave {actual:?}"),
        }
        if get(case, "parses") == &JsValue::Bool(true) {
            repaired_count += 1;
        } else {
            failed_count += 1;
        }
        let raws = [
            r#"{"type":"content_block_start","index":3,"content_block":{"type":"tool_use","id":"toolu_r","name":"fix","input":{}}}"#,
            "",
            r#"{"type":"content_block_stop","index":3}"#,
            r#"{"type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":1}}"#,
        ];
        let mut state = StreamState::default();
        let mut events = Vec::new();
        for (i, raw) in raws.iter().enumerate() {
            let raw = if i == 1 {
                let mut delta = pilotdeck_protocol::JsObject::new();
                delta.insert("type".into(), "input_json_delta".into());
                delta.insert("partial_json".into(), JsValue::String(input.clone()));
                let mut event = pilotdeck_protocol::JsObject::new();
                event.insert("type".into(), "content_block_delta".into());
                event.insert("index".into(), JsValue::Number(3.0));
                event.insert("delta".into(), JsValue::Object(delta));
                JsValue::Object(event)
            } else {
                parse_json(raw).unwrap()
            };
            events.extend(normalize_stream_event(&raw, &mut state));
        }
        let JsValue::Array(expected) = get(case, "events") else {
            panic!("events")
        };
        let actual: Vec<String> = events.iter().map(json).collect();
        let expected: Vec<String> = expected.iter().map(json).collect();
        assert_eq!(actual, expected, "events {label}");
    }
    assert_eq!((repaired_count, failed_count), (74, 12));
}

/// Ad-hoc differential against a node-generated corpus
/// (`PDRS_JSONREPAIR_CORPUS=<file>`; see RESULT 修复轮 1).
#[test]
#[ignore = "needs PDRS_JSONREPAIR_CORPUS"]
fn differential_corpus_from_env() {
    let path = std::env::var("PDRS_JSONREPAIR_CORPUS").expect("PDRS_JSONREPAIR_CORPUS");
    let corpus = parse_json(&std::fs::read_to_string(path).unwrap()).unwrap();
    let JsValue::Array(ref cases) = corpus else {
        panic!()
    };
    let mut mismatches = 0;
    for case in cases {
        let input = get(case, "input").as_string().unwrap();
        let actual = jsonrepair(input.units()).ok();
        let expected = get(case, "repaired")
            .as_string()
            .map(|s| s.units().to_vec());
        if actual != expected {
            mismatches += 1;
            eprintln!(
                "MISMATCH {:?}: rust={:?} ts={:?}",
                String::from_utf16_lossy(input.units()),
                actual.map(|a| String::from_utf16_lossy(&a)),
                expected.map(|e| String::from_utf16_lossy(&e))
            );
        }
    }
    eprintln!(
        "differential: {} cases, {mismatches} mismatches",
        cases.len()
    );
    assert_eq!(mismatches, 0);
}
