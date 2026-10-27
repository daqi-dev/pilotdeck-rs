//! Config change diff/classification replayed from the real TypeScript baseline
//! (`tests/fixtures/config-changes/generate.mts`). Every expected path, class
//! order and runPolicy flag in `oracle.json` comes from `classifyChanges.ts`.
#[path = "common/baseline.rs"]
mod baseline;
use pilotdeck_gateway::config::changes::{
    ChangeClass, classify_config_changes, diff_config_snapshots, is_run_policy_only_change,
};
use pilotdeck_protocol::{JsObject, JsString, JsValue};
use std::collections::HashSet;
use std::fs;

const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/config-changes");
const SOURCE_COMMIT: &str = "cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85";
const SOURCE: &str = "src/pilot/config/classifyChanges.ts";
const SOURCE_SHA256: &str = "ab2f844a1cee62603e452de7bcf280ee8a4b524979c77d1b11745867ae2a8fbf";
fn expected_names(key: &str) -> Vec<String> {
    let manifest =
        pilotdeck_protocol::parse_json(include_str!("fixtures/config-changes/case-names.json"))
            .expect("#911 valid case-name manifest");
    class_texts(field(&manifest, key))
}

static UNDEFINED: JsValue = JsValue::Undefined;

fn field<'a>(value: &'a JsValue, key: &str) -> &'a JsValue {
    value
        .as_object()
        .and_then(|object| object.get_property(key))
        .unwrap_or(&UNDEFINED)
}

fn text(value: &JsValue) -> String {
    value
        .as_string()
        .expect("#911 string")
        .to_utf8()
        .expect("#911 fixture text is valid UTF-8")
}

fn strings(value: &JsValue) -> Vec<JsString> {
    match value {
        JsValue::Array(items) => items
            .iter()
            .map(|item| item.as_string().expect("#911 path string").clone())
            .collect(),
        _ => panic!("#911 expected an array of strings"),
    }
}

fn class_texts(value: &JsValue) -> Vec<String> {
    strings(value)
        .iter()
        .map(|class| class.to_utf8().expect("#911 class name"))
        .collect()
}

fn class_names(classes: Vec<ChangeClass>) -> Vec<String> {
    classes
        .iter()
        .map(|class| class.as_str().to_owned())
        .collect()
}

/// `oracle.json` is JSON, so the generator encodes the JavaScript-only values
/// (`undefined`, `NaN`, `Infinity`, `-0`) as a single-key `{"$": tag}` object,
/// and an explicit `[[Prototype]]` as `{"$": "prototype", ...}`. A real fixture
/// object carrying a `$` key is rejected by the generator.
fn decode(value: &JsValue) -> JsValue {
    let JsValue::Object(object) = value else {
        return match value {
            JsValue::Array(items) => JsValue::Array(items.iter().map(decode).collect()),
            other => other.clone(),
        };
    };
    if let Some(tag) = object.get("$").and_then(JsValue::as_string) {
        match tag.to_utf8().expect("#911 tag").as_str() {
            "prototype" => {
                let prototype = decode(
                    object
                        .get("prototype")
                        .expect("#911 prototype tag carries a prototype"),
                );
                let JsValue::Object(entries) = object
                    .get("value")
                    .expect("#911 prototype tag carries own entries")
                else {
                    panic!("#911 prototype tag value must be an object");
                };
                let mut decoded = JsObject::new();
                for (key, value) in entries.entries() {
                    decoded.insert(key.clone(), decode(value));
                }
                decoded.set_prototype(prototype);
                return JsValue::Object(decoded);
            }
            other if object.len() == 1 => {
                return match other {
                    "undefined" => JsValue::Undefined,
                    "NaN" => JsValue::Number(f64::NAN),
                    "Infinity" => JsValue::Number(f64::INFINITY),
                    "-Infinity" => JsValue::Number(f64::NEG_INFINITY),
                    "-0" => JsValue::Number(-0.0),
                    other => panic!("#911 unknown value tag {other}"),
                };
            }
            other => panic!("#911 unknown value tag {other}"),
        }
    }
    let mut decoded = JsObject::new();
    for (key, value) in object.entries() {
        decoded.insert(key.clone(), decode(value));
    }
    JsValue::Object(decoded)
}

fn oracle() -> JsValue {
    let text = fs::read_to_string(format!("{FIXTURES}/oracle.json")).expect("#911 oracle.json");
    assert!(
        !text.contains(".generate-"),
        "#911 oracle.json leaks a generation path"
    );
    std::panic::catch_unwind(|| baseline::oracle(&text)).expect("#911 baseline provenance")
}

#[test]
fn real_ts_snapshot_pairs_match_diff_and_classes() {
    let oracle = oracle();
    assert_eq!(
        text(field(&oracle, "sourceCommit")),
        SOURCE_COMMIT,
        "#911 source commit"
    );
    let hashes = field(&oracle, "sourceSha256");
    assert_eq!(
        text(field(hashes, SOURCE)),
        SOURCE_SHA256,
        "#911 source hash"
    );

    let pairs = match field(&oracle, "pairs") {
        JsValue::Array(pairs) => pairs,
        _ => panic!("#911 pairs array"),
    };
    let expected = expected_names("pairs");
    assert_eq!(pairs.len(), expected.len(), "#911 exact pair count");
    let actual_names: HashSet<_> = pairs.iter().map(|pair| text(field(pair, "name"))).collect();
    let expected_names: HashSet<_> = expected.into_iter().collect();
    assert_eq!(actual_names, expected_names, "#911 exact pair names");
    let mut names = HashSet::new();
    let mut seen = HashSet::new();

    for pair in pairs {
        let name = text(field(pair, "name"));
        assert!(
            names.insert(name.clone()),
            "#911 duplicate pair name {name}"
        );

        let previous = decode(field(pair, "previous"));
        let next = decode(field(pair, "next"));
        let expected_paths = strings(field(pair, "changedPaths"));
        let expected_classes = class_texts(field(pair, "changeClasses"));
        let expected_run_policy_only = field(pair, "runPolicyOnly")
            .as_bool()
            .expect("#911 runPolicyOnly bool");

        let actual_paths = diff_config_snapshots(&previous, &next);
        assert_eq!(actual_paths, expected_paths, "#911 {name}: changedPaths");
        assert_eq!(
            class_names(classify_config_changes(&actual_paths)),
            expected_classes,
            "#911 {name}: changeClasses"
        );
        assert_eq!(
            is_run_policy_only_change(&actual_paths),
            expected_run_policy_only,
            "#911 {name}: runPolicyOnly"
        );
        // The baseline's recorded paths must classify to its recorded classes too.
        assert_eq!(
            class_names(classify_config_changes(&expected_paths)),
            expected_classes,
            "#911 {name}: classify(recorded changedPaths)"
        );
        seen.extend(expected_classes);
    }

    for class in [
        "runtime-live",
        "next-request",
        "next-runtime",
        "restart-required",
    ] {
        assert!(
            seen.contains(class),
            "#911 fixture set never produced {class}"
        );
    }
}

#[test]
fn real_ts_direct_classify_cases_match() {
    let oracle = oracle();
    let cases = match field(&oracle, "classify") {
        JsValue::Array(cases) => cases,
        _ => panic!("#911 classify array"),
    };
    let expected = expected_names("classify");
    assert_eq!(cases.len(), expected.len(), "#911 exact classify count");
    let actual_names: HashSet<_> = cases.iter().map(|case| text(field(case, "name"))).collect();
    let expected_names: HashSet<_> = expected.into_iter().collect();
    assert_eq!(actual_names, expected_names, "#911 exact classify names");
    let mut names = HashSet::new();

    for case in cases {
        let name = text(field(case, "name"));
        assert!(
            names.insert(name.clone()),
            "#911 duplicate classify name {name}"
        );
        let paths = strings(field(case, "paths"));
        assert_eq!(
            class_names(classify_config_changes(&paths)),
            class_texts(field(case, "classes")),
            "#911 {name}: classes"
        );
        assert_eq!(
            is_run_policy_only_change(&paths),
            field(case, "runPolicyOnly")
                .as_bool()
                .expect("#911 runPolicyOnly bool"),
            "#911 {name}: runPolicyOnly"
        );
    }
}

#[test]
#[ignore = "requires the fixed TS baseline and Node v22.22.0"]
fn config_changes_regenerates_identically() {
    baseline::regenerates_identically("config-changes");
}
