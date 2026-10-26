//! Real TS oracle for A6.6-edgeclaw-15 dream pure helpers.
use pilotdeck_gateway::runtime::memory::edgeclaw::dream::helpers::{
    create_dream_trace, mutation, push_step, select_user_note_window, sort_entries,
    sort_project_metas, to_dream_meta_input, to_dream_record_input, to_header_input,
    to_refined_candidate, validate_exclusive_clusters, validate_general_project_merge_groups,
};
use pilotdeck_protocol::{JsObject, JsString, JsValue, parse_json, stringify_json};
use std::collections::HashSet;

#[path = "common/baseline.rs"]
mod baseline;
const ORACLE: &str = include_str!("fixtures/edgeclaw-dream/oracle.json");

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
        "createDreamTrace" => "trace",
        "pushStep" => "steps",
        "mutation" => "mutation",
        "sortEntries" | "sortProjectMetas" => "sort",
        "toDreamMetaInput" | "toDreamRecordInput" | "toHeaderInput" | "toRefinedCandidate" => {
            "inputs"
        }
        "validateExclusiveClusters" => "clusters",
        "selectUserNoteWindow" => "window",
        "validateGeneralProjectMergeGroups" => "merge-groups",
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

fn arg(case: &JsValue, index: usize) -> &JsValue {
    &args(case)[index]
}

fn optional_arg(case: &JsValue, index: usize) -> Option<&JsValue> {
    args(case).get(index)
}

fn text(case: &JsValue, index: usize) -> &JsString {
    arg(case, index).as_string().expect("string argument")
}

fn number(case: &JsValue, index: usize) -> f64 {
    arg(case, index).as_number().expect("number argument")
}

fn object(case: &JsValue, index: usize) -> &JsObject {
    arg(case, index).as_object().expect("object argument")
}

fn optional_object(case: &JsValue, index: usize) -> Option<&JsObject> {
    optional_arg(case, index).and_then(JsValue::as_object)
}

fn call(case: &JsValue) -> JsValue {
    let name = case
        .get("fn")
        .unwrap()
        .as_string()
        .unwrap()
        .to_utf8()
        .unwrap();
    match name.as_str() {
        "createDreamTrace" => JsValue::Object(create_dream_trace(
            text(case, 0),
            text(case, 1),
            text(case, 2),
        )),
        "pushStep" => {
            let empty = JsObject::new();
            let mut trace = object(case, 0).clone();
            push_step(
                &mut trace,
                text(case, 1),
                text(case, 2),
                text(case, 3),
                text(case, 4),
                text(case, 5),
                optional_object(case, 6).unwrap_or(&empty),
            );
            JsValue::Object(trace)
        }
        "mutation" => {
            let empty = JsObject::new();
            JsValue::Object(mutation(
                text(case, 0),
                text(case, 1),
                number(case, 2),
                number(case, 3),
                optional_object(case, 4).unwrap_or(&empty),
            ))
        }
        "sortEntries" => JsValue::Array(
            sort_entries(arg(case, 0))
                .unwrap()
                .into_iter()
                .map(JsValue::Object)
                .collect(),
        ),
        "sortProjectMetas" => JsValue::Array(
            sort_project_metas(arg(case, 0))
                .unwrap()
                .into_iter()
                .map(JsValue::Object)
                .collect(),
        ),
        "toDreamMetaInput" => JsValue::Object(to_dream_meta_input(object(case, 0))),
        "toDreamRecordInput" => JsValue::Object(to_dream_record_input(object(case, 0))),
        "toHeaderInput" => JsValue::Object(to_header_input(object(case, 0))),
        "toRefinedCandidate" => JsValue::Object(to_refined_candidate(object(case, 0))),
        "validateExclusiveClusters" => {
            let allowed: HashSet<JsString> = items(arg(case, 1))
                .iter()
                .filter_map(JsValue::as_string)
                .cloned()
                .collect();
            JsValue::Object(validate_exclusive_clusters(
                arg(case, 0),
                &allowed,
                number(case, 2),
            ))
        }
        "selectUserNoteWindow" => JsValue::Object(select_user_note_window(arg(case, 0)).unwrap()),
        "validateGeneralProjectMergeGroups" => {
            JsValue::Object(validate_general_project_merge_groups(object(case, 0)))
        }
        other => panic!("missing dispatch for {other}"),
    }
}

fn replay(wanted: &str) {
    let oracle = oracle();
    let mut seen = 0usize;
    for case in items(oracle.get("cases").unwrap()) {
        let name = case
            .get("fn")
            .unwrap()
            .as_string()
            .unwrap()
            .to_utf8()
            .unwrap();
        if group(&name) != wanted {
            continue;
        }
        seen += 1;
        let decoded = decode(case);
        let actual = call(&decoded);
        let expected = decoded.get("expected").unwrap();
        assert_eq!(
            wire(&actual),
            wire(expected),
            "{wanted}/ {name} args={:?}",
            decoded.get("args").unwrap()
        );
    }
    assert!(seen > 0, "no {wanted} cases in the oracle");
}

#[test]
fn trace_matches_ts() {
    replay("trace");
}

#[test]
fn steps_match_ts() {
    replay("steps");
}

#[test]
fn mutation_matches_ts() {
    replay("mutation");
}

#[test]
fn sort_matches_ts() {
    replay("sort");
}

#[test]
fn inputs_match_ts() {
    replay("inputs");
}

#[test]
fn clusters_match_ts() {
    replay("clusters");
}

#[test]
fn window_matches_ts() {
    replay("window");
}

#[test]
fn merge_groups_match_ts() {
    replay("merge-groups");
}

#[test]
#[ignore = "requires fixed PDRS_BASELINE with Node 22 and tsx"]
fn fixture_regenerates_identically() {
    baseline::regenerates_identically("edgeclaw-dream");
}
