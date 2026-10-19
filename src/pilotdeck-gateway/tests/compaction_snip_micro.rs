//! A6.6-pr7 model-free compaction contract tests. Expected message lists,
//! counts and bytes come from the real fixed-baseline SnipEngine /
//! MicroCompactionEngine via tests/fixtures/compaction-snip-micro/generate.mts.
#[path = "common/baseline.rs"]
mod baseline;

use pilotdeck_gateway::runtime::context::compaction::{
    MicroCompactionEngine, MicroCompactionEngineOptions, MicroCompactionInput, SnipCallOptions,
    SnipEngine, SnipEngineOptions, is_snip_boundary_message,
};
use pilotdeck_protocol::{JsObject, JsValue, stringify_json};

const ORACLE: &str = include_str!("fixtures/compaction-snip-micro/oracle.json");
static UNDEFINED: JsValue = JsValue::Undefined;

fn field<'a>(value: &'a JsValue, name: &str) -> &'a JsValue {
    value.get(name).unwrap_or(&UNDEFINED)
}

fn s(value: &JsValue) -> String {
    value
        .as_string()
        .expect("expected a string")
        .to_utf8()
        .expect("expected UTF-8")
}

fn array(value: &JsValue) -> &[JsValue] {
    match value {
        JsValue::Array(items) => items,
        _ => panic!("expected an array"),
    }
}

fn object(fields: impl IntoIterator<Item = (&'static str, JsValue)>) -> JsValue {
    let mut value = JsObject::new();
    for (key, child) in fields {
        value.insert(key.into(), child);
    }
    JsValue::Object(value)
}

fn optional_f64(value: &JsValue) -> Option<f64> {
    value.as_number()
}

fn names(value: &JsValue) -> Option<Vec<String>> {
    match value {
        JsValue::Array(items) => Some(items.iter().map(s).collect()),
        _ => None,
    }
}

fn snip_options(value: &JsValue) -> SnipEngineOptions {
    SnipEngineOptions {
        keep_head_turns: optional_f64(field(value, "keepHeadTurns")),
        keep_tail_turns: optional_f64(field(value, "keepTailTurns")),
        enabled: field(value, "enabled").as_bool(),
        protected_tool_names: names(field(value, "protectedToolNames")),
    }
}

fn snip_call_options(value: &JsValue) -> SnipCallOptions {
    SnipCallOptions {
        target_tokens: optional_f64(field(value, "targetTokens")),
        target_total_tokens: optional_f64(field(value, "targetTotalTokens")),
    }
}

fn micro_options(value: &JsValue) -> MicroCompactionEngineOptions {
    MicroCompactionEngineOptions {
        keep_latest: optional_f64(field(value, "keepLatest")),
        trim_to_bytes: optional_f64(field(value, "trimToBytes")),
        trim_to_tokens: optional_f64(field(value, "trimToTokens")),
        protected_tool_names: names(field(value, "protectedToolNames")),
    }
}

fn micro_input<'a>(messages: &'a [JsValue], value: &JsValue) -> MicroCompactionInput<'a> {
    let mut input = MicroCompactionInput::new(messages);
    input.now_ms = optional_f64(field(value, "nowMs"));
    input.idle_ms = optional_f64(field(value, "idleMs"));
    input.trim_to_bytes = optional_f64(field(value, "trimToBytes"));
    input.trim_to_tokens = optional_f64(field(value, "trimToTokens"));
    input.keep_latest = optional_f64(field(value, "keepLatest"));
    input.protected_tool_names = match value.get("protectedToolNames") {
        None | Some(JsValue::Undefined) => None,
        Some(JsValue::Null) => Some(None),
        Some(JsValue::Array(_)) => Some(names(field(value, "protectedToolNames"))),
        Some(_) => panic!("protectedToolNames must be null or an array"),
    };
    input
}

fn run(scenario: &JsValue) -> JsValue {
    let input = array(field(scenario, "input"));
    match s(field(scenario, "engine")).as_str() {
        "snip" => {
            let result = SnipEngine::new(snip_options(field(scenario, "options")))
                .snip(input, snip_call_options(field(scenario, "callOptions")))
                .expect("tokenize snip input");
            object([
                ("messages", JsValue::Array(result.messages)),
                ("applied", JsValue::Bool(result.applied)),
                ("turnsSnipped", JsValue::Number(result.turns_snipped as f64)),
                (
                    "danglingToolCallIds",
                    JsValue::Array(
                        result
                            .dangling_tool_call_ids
                            .into_iter()
                            .map(JsValue::String)
                            .collect(),
                    ),
                ),
            ])
        }
        "micro" => {
            let engine = MicroCompactionEngine::new(micro_options(field(scenario, "options")));
            let result = engine
                .apply(&micro_input(input, field(scenario, "inputOptions")))
                .expect("tokenize micro input");
            object([
                ("messages", JsValue::Array(result.messages)),
                ("rewritten", JsValue::Number(result.rewritten as f64)),
                ("rewrittenBytes", JsValue::Number(result.rewritten_bytes)),
                (
                    "toolCallIds",
                    JsValue::Array(
                        result
                            .tool_call_ids
                            .into_iter()
                            .map(JsValue::String)
                            .collect(),
                    ),
                ),
                (
                    "appliedTrigger",
                    JsValue::String(result.applied_trigger.into()),
                ),
            ])
        }
        other => panic!("unknown engine {other}"),
    }
}

#[test]
fn real_ts_snip_and_micro_scenarios_match_byte_for_byte() {
    let oracle = baseline::oracle(ORACLE);
    assert_eq!(s(field(&oracle, "locale")), "en-US");
    let scenarios = array(field(&oracle, "scenarios"));
    assert_eq!(scenarios.len(), 29, "scenario count");
    let mut seen = std::collections::HashSet::new();
    for scenario in scenarios {
        let name = s(field(scenario, "name"));
        assert!(seen.insert(name.clone()), "duplicate scenario {name}");
        let input_before = field(scenario, "input").clone();
        let actual = run(scenario);
        assert_eq!(
            field(scenario, "input"),
            &input_before,
            "scenario {name} must not mutate its input"
        );
        let expected = field(scenario, "result");
        let actual_json = stringify_json(&actual)
            .unwrap()
            .expect("serializable actual");
        let expected_json = stringify_json(expected)
            .unwrap()
            .expect("serializable expected");
        assert_eq!(
            actual_json, expected_json,
            "scenario {name} diverged from the real TS output"
        );
        assert_eq!(
            &actual, expected,
            "scenario {name} differs after ordered JSON comparison"
        );
    }
}

#[test]
fn snip_boundary_recognition_matches_engine_output() {
    let oracle = baseline::oracle(ORACLE);
    for scenario in array(field(&oracle, "scenarios")) {
        if s(field(scenario, "engine")) != "snip" {
            continue;
        }
        let messages = array(field(field(scenario, "result"), "messages"));
        let boundary = messages.iter().find(|message| {
            message.get("content").is_some_and(|content| match content {
                JsValue::Array(blocks) => blocks.iter().any(|block| {
                    block
                        .get("text")
                        .and_then(JsValue::as_string)
                        .is_some_and(|text| {
                            text.to_utf8()
                                .is_ok_and(|text| text.starts_with("<snip-boundary"))
                        })
                }),
                _ => false,
            })
        });
        if let Some(boundary) = boundary {
            assert!(
                is_snip_boundary_message(boundary),
                "scenario {} produced an unrecognized boundary",
                s(field(scenario, "name"))
            );
        }
    }
}

#[test]
fn project_snipped_view_returns_input_verbatim_when_no_target_is_given() {
    // The baseline's unconstrained tail walk keeps every turn, so the
    // projection is the input for conversations below the head+tail window
    // and for all unconstrained inputs. The fixture pins the engine path.
    let oracle = baseline::oracle(ORACLE);
    let scenario = array(field(&oracle, "scenarios"))
        .iter()
        .find(|scenario| s(field(scenario, "name")) == "unconstrained-head-tail-noop")
        .expect("fixture scenario");
    let input = array(field(scenario, "input"));
    let projected = pilotdeck_gateway::runtime::context::compaction::project_snipped_view(
        input,
        SnipEngineOptions::default(),
    )
    .expect("tokenize projection");
    assert_eq!(
        stringify_json(&JsValue::Array(projected)).unwrap(),
        stringify_json(&JsValue::Array(input.to_vec())).unwrap()
    );
}
