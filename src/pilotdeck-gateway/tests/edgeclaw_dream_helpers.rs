//! Real TS oracle for A6.6-edgeclaw-15 dream pure helpers.
use pilotdeck_gateway::runtime::memory::edgeclaw::FileMemoryError;
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
    // Iterative: one golden case expands a 3000-level reason array for the
    // `String(value)` RangeError, which overflows a recursive decode on the
    // default test-thread stack. `JsObject` insertion order is preserved.
    enum Task<'a> {
        Visit(&'a JsValue),
        Array(usize),
        Object(&'a JsObject),
    }
    let mut tasks = vec![Task::Visit(value)];
    let mut out: Vec<JsValue> = Vec::new();
    while let Some(task) = tasks.pop() {
        match task {
            Task::Visit(JsValue::Array(items)) => {
                tasks.push(Task::Array(items.len()));
                tasks.extend(items.iter().rev().map(Task::Visit));
            }
            Task::Visit(JsValue::Object(object)) => {
                if object.len() == 1
                    && matches!(object.get("$undefined"), Some(JsValue::Bool(true)))
                {
                    out.push(JsValue::Undefined);
                } else if object.len() == 1
                    && let Some(JsValue::String(kind)) = object.get("$number")
                    && let Ok(kind) = kind.to_utf8()
                {
                    out.push(JsValue::Number(match kind.as_str() {
                        "NaN" => f64::NAN,
                        "Infinity" => f64::INFINITY,
                        "-Infinity" => f64::NEG_INFINITY,
                        "-0" => -0.0,
                        other => other.parse().expect("number sentinel"),
                    }));
                } else if object.len() == 1
                    && let Some(JsValue::Number(depth)) = object.get("$deepArray")
                    && depth.is_finite()
                    && *depth >= 0.0
                {
                    // Lossless stand-in for `[[…['x']…]]` (3000 deep in the
                    // golden set): pretty-printed nesting would be ~14 MB.
                    let mut deep = JsValue::String("x".into());
                    for _ in 0..(*depth as usize) {
                        deep = JsValue::Array(vec![deep]);
                    }
                    out.push(deep);
                } else {
                    tasks.push(Task::Object(object));
                    tasks.extend(
                        object
                            .entries()
                            .iter()
                            .rev()
                            .map(|entry| Task::Visit(&entry.1)),
                    );
                }
            }
            Task::Visit(other) => out.push(other.clone()),
            Task::Array(len) => {
                let items = out.split_off(out.len() - len);
                out.push(JsValue::Array(items));
            }
            Task::Object(object) => {
                let values = out.split_off(out.len() - object.len());
                let mut decoded = JsObject::new();
                for ((key, _), item) in object.entries().iter().zip(values) {
                    decoded.insert(key.clone(), item);
                }
                out.push(JsValue::Object(decoded));
            }
        }
    }
    out.pop().expect("decode produces one value")
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

fn call(case: &JsValue) -> Result<JsValue, FileMemoryError> {
    let name = case
        .get("fn")
        .unwrap()
        .as_string()
        .unwrap()
        .to_utf8()
        .unwrap();
    Ok(match name.as_str() {
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
            )?)
        }
        "selectUserNoteWindow" => JsValue::Object(select_user_note_window(arg(case, 0)).unwrap()),
        "validateGeneralProjectMergeGroups" => {
            JsValue::Object(validate_general_project_merge_groups(object(case, 0))?)
        }
        other => panic!("missing dispatch for {other}"),
    })
}

fn expected_error(case: &JsValue) -> (String, String) {
    let thrown = case
        .get("throws")
        .expect("case has neither expected nor throws");
    let name = thrown
        .get("name")
        .unwrap()
        .as_string()
        .unwrap()
        .to_utf8()
        .unwrap();
    let message = thrown
        .get("message")
        .unwrap()
        .as_string()
        .unwrap()
        .to_utf8()
        .unwrap();
    (name, message)
}

/// Golden args can be 3000 arrays deep (the `$deepArray` case); keep failure
/// messages short so one surviving-mutant log stays readable.
fn args_summary(case: &JsValue) -> String {
    let text = format!("{:?}", case.get("args").unwrap());
    let mut chars = text.chars();
    let short: String = chars.by_ref().take(300).collect();
    if chars.next().is_some() {
        format!("{short}…")
    } else {
        short
    }
}

fn replay(wanted: &str) {
    let oracle = oracle();
    let mut seen = 0usize;
    // Stable per-function case IDs: `<fn>#<n>` where `n` counts the fn's
    // entries in oracle order. Negative controls bind one exact failure to one
    // ID, so a mutant that trips an earlier sibling case cannot be mistaken
    // for a kill of the case it claims to cover. The fixture exists to freeze
    // order; a regenerated fixture that reorders cases makes the control stale
    // (the script checks its target signature before running).
    let mut positions: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
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
        let index = *positions.entry(name.clone()).or_default();
        positions.insert(name.clone(), index + 1);
        let case_id = format!("{name}#{index}");
        seen += 1;
        let decoded = decode(case);
        let actual = call(&decoded);
        if decoded.get("throws").is_some() {
            let (error_name, message) = expected_error(&decoded);
            match actual {
                Err(FileMemoryError::Type(actual)) => {
                    assert_eq!(error_name, "TypeError", "{wanted}/ {name} case={case_id}");
                    assert_eq!(actual, message, "{wanted}/ {name} case={case_id}");
                }
                Err(FileMemoryError::Range(actual)) => {
                    assert_eq!(error_name, "RangeError", "{wanted}/ {name} case={case_id}");
                    assert_eq!(actual, message, "{wanted}/ {name} case={case_id}");
                }
                other => panic!(
                    "{wanted}/ {name} case={case_id} expected {error_name} {message:?}, got {other:?} args={}",
                    args_summary(&decoded)
                ),
            }
            continue;
        }
        let expected = decoded.get("expected").unwrap();
        assert_eq!(
            wire(&actual.unwrap()),
            wire(expected),
            "{wanted}/ {name} case={case_id} args={}",
            args_summary(&decoded)
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
