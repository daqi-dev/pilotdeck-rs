//! One difference registry across the integration and both private caller modules.
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

const SOURCES: [&str; 3] = [
    include_str!("../network_timing.rs"),
    include_str!("network_timing_mcp.rs"),
    include_str!("network_timing_hooks.rs"),
];
const WIRING: [&str; 2] = [
    include_str!("../../src/runtime/mcp/http.rs"),
    include_str!("../../src/runtime/hooks/exec.rs"),
];
const TABLE: &str = include_str!("../fixtures/network-timing/known_differences.json");
pub fn validate(oracle: &Value) -> Vec<Value> {
    validate_parts(
        oracle,
        &serde_json::from_str(TABLE).unwrap(),
        &SOURCES,
        &WIRING,
    )
}
fn validate_parts(
    oracle: &Value,
    table: &Value,
    sources: &[&str; 3],
    wiring: &[&str; 2],
) -> Vec<Value> {
    for (source, name) in wiring.iter().zip(["mcp", "hooks"]) {
        let declaration = format!(
            "#[cfg(test)]\n#[path = \"../../../tests/common/network_timing_{name}.rs\"]\nmod network_timing_tests;"
        );
        assert_eq!(
            source.matches(&declaration).count(),
            1,
            "private caller cfg(test) wiring: {name}"
        );
        let before = source.split(&declaration).next().unwrap().trim_end();
        let previous = before
            .lines()
            .rev()
            .map(str::trim)
            .find(|line| !line.is_empty() && !line.starts_with("//"))
            .unwrap_or("");
        assert!(
            !previous.starts_with("#["),
            "private caller has extra cfg/attribute: {name}"
        );
    }
    let mut rows = BTreeMap::new();
    for row in oracle["cases"].as_array().unwrap() {
        assert!(
            rows.insert(row["id"].as_str().unwrap(), row).is_none(),
            "duplicate oracle id"
        );
    }
    assert_eq!(rows.len(), 336, "complete timing matrix");
    let mut ignored = BTreeMap::new();
    let mut tests = BTreeMap::new();
    for source in sources {
        let mut reason = None;
        let mut attributes = Vec::new();
        for line in source.lines().map(str::trim) {
            if line.starts_with("#[") {
                attributes.push(line);
            }
            if line.starts_with("#[ignore") {
                assert!(reason.is_none(), "duplicate ignore attribute");
                reason = Some(
                    line.strip_prefix("#[ignore = \"")
                        .and_then(|s| s.strip_suffix("\"]"))
                        .expect("literal ignore reason"),
                );
            }
            if line.starts_with("fn ") || line.starts_with("async fn ") {
                let name = line
                    .trim_start_matches("async ")
                    .strip_prefix("fn ")
                    .unwrap()
                    .split('(')
                    .next()
                    .unwrap();
                if name == "known_differences_remain_exact_and_ignored" {
                    assert_eq!(
                        attributes,
                        ["#[tokio::test(start_paused = true)]"],
                        "difference replay guard must be executable"
                    );
                }
                if let Some(reason) = reason.take() {
                    if name == "network_timing_regenerates_identically" {
                        assert_eq!(reason, "requires fixed TS baseline and Node v22.22.0");
                    } else {
                        assert!(
                            ignored.insert(name, reason).is_none(),
                            "duplicate ignored test"
                        );
                    }
                }
                if let Some(rest) = source.split(&format!("async fn {name}() {{")).nth(1) {
                    let body = rest.split("\n}").next().unwrap().trim();
                    if let Some(id) = body
                        .strip_prefix("check(\"")
                        .and_then(|s| s.strip_suffix("\").await;"))
                    {
                        let expected_test = if id.contains("/io-headers/") {
                            "#[tokio::test]"
                        } else {
                            "#[tokio::test(start_paused = true)]"
                        };
                        assert_eq!(
                            attributes.iter().filter(|a| **a == expected_test).count(),
                            1,
                            "cell must be an executable test: {id}"
                        );
                        assert!(
                            attributes
                                .iter()
                                .all(|a| *a == expected_test || a.starts_with("#[ignore = ")),
                            "unexpected cell cfg/attribute: {id}"
                        );
                        assert_eq!(name, id.replace(['/', '-'], "_"), "cell test/id binding");
                        assert!(
                            tests.insert(id.to_owned(), name.to_owned()).is_none(),
                            "duplicate cell test"
                        );
                    }
                }
            }
            if !line.starts_with("#[") && !line.is_empty() && !line.starts_with("//") {
                attributes.clear();
            }
        }
        assert!(reason.is_none(), "unbound ignore attribute");
    }
    assert_eq!(
        tests.keys().map(String::as_str).collect::<BTreeSet<_>>(),
        rows.keys().copied().collect(),
        "all oracle cells wired exactly once"
    );
    let mut registered = BTreeMap::new();
    let mut ids = BTreeSet::new();
    for entry in table["cells"].as_array().unwrap() {
        let id = entry["id"].as_str().unwrap();
        let name = entry["test"].as_str().unwrap();
        assert!(ids.insert(id), "duplicate difference id");
        assert_eq!(
            Some(name),
            tests.get(id).map(String::as_str),
            "ignored cell must assert TS oracle"
        );
        let category = &table["categories"][entry["category"].as_str().unwrap()];
        let reason = category["ignoreReason"].as_str().expect("known category");
        assert!(
            registered.insert(name, reason).is_none(),
            "duplicate difference test"
        );
        assert_eq!(
            entry["ts"], rows[id]["observable"],
            "recorded TS observation: {id}"
        );
        assert_ne!(
            entry["ts"], entry["rust"],
            "resolved difference must be unignored: {id}"
        );
    }
    assert_eq!(
        ignored, registered,
        "ignored cells and difference table must match exactly"
    );
    table["cells"].as_array().unwrap().clone()
}

/// In-memory source/table negatives: no source file or frozen oracle is changed.
#[allow(dead_code)]
pub fn negative_controls(oracle: &Value) {
    let table: Value = serde_json::from_str(TABLE).unwrap();
    validate_parts(oracle, &table, &SOURCES, &WIRING);
    let mut count = 0;
    let mut reject =
        |label: &str, oracle: &Value, table: &Value, sources: [&str; 3], wiring: [&str; 2]| {
            let panic =
                std::panic::catch_unwind(|| validate_parts(oracle, table, &sources, &wiring))
                    .expect_err(&format!("registry negative was accepted: {label}"));
            let message = panic
                .downcast_ref::<String>()
                .map(String::as_str)
                .or_else(|| panic.downcast_ref::<&str>().copied())
                .expect("registry panic text");
            let anchor = match label.split(':').next().unwrap() {
                "missing-test" | "wrong-clock" => "cell must be an executable test",
                "disabled-test" => "unexpected cell cfg/attribute",
                "extra-ignore" | "missing-difference" => {
                    "ignored cells and difference table must match exactly"
                }
                "wrong-cell" => "cell test/id binding",
                "missing-wiring" => "private caller cfg(test) wiring",
                "disabled-wiring" => "private caller has extra cfg/attribute",
                "difference-ts" => "recorded TS observation",
                "difference-rust" => "resolved difference must be unignored",
                "difference-test" => "ignored cell must assert TS oracle",
                "difference-category" => "known category",
                "missing-oracle-cell" => "complete timing matrix",
                "missing-replay-guard" | "disabled-replay-guard" => {
                    "difference replay guard must be executable"
                }
                _ => panic!("unbound registry negative: {label}"),
            };
            assert!(
                message.contains(anchor),
                "wrong registry panic for {label}: expected {anchor}, got {message}"
            );
            validate_parts(
                &serde_json::from_str(include_str!("../fixtures/network-timing/oracle.json"))
                    .unwrap(),
                &serde_json::from_str(TABLE).unwrap(),
                &SOURCES,
                &WIRING,
            );
            println!("REGISTRY_NEGATIVE {label}: rejected; restored PASS");
            count += 1;
        };
    for (index, id) in [
        "web_fetch/headers/fulfilled/continuation-first",
        "mcp_get/headers/fulfilled/continuation-first",
        "hooks/headers/fulfilled/continuation-first",
    ]
    .into_iter()
    .enumerate()
    {
        let name = id.replace(['/', '-'], "_");
        let declaration = format!("#[tokio::test(start_paused = true)]\nasync fn {name}() {{");
        let changed = SOURCES[index].replace(
            &declaration,
            &format!(
                "#[cfg(any())]\n\n// attribute remains attached across comments\n{declaration}"
            ),
        );
        let mut sources = SOURCES;
        sources[index] = &changed;
        reject(
            &format!("disabled-test:{id}:separated"),
            oracle,
            &table,
            sources,
            WIRING,
        );
        for (label, replacement) in [
            ("missing-test", format!("async fn {name}() {{")),
            (
                "disabled-test",
                format!("#[tokio::test(start_paused = true)]\n#[cfg(any())]\nasync fn {name}() {{"),
            ),
            (
                "wrong-clock",
                format!("#[tokio::test]\nasync fn {name}() {{"),
            ),
            (
                "extra-ignore",
                format!(
                    "#[tokio::test(start_paused = true)]\n#[ignore = \"unregistered\"]\nasync fn {name}() {{"
                ),
            ),
        ] {
            assert_eq!(SOURCES[index].matches(&declaration).count(), 1);
            let changed = SOURCES[index].replace(&declaration, &replacement);
            let mut sources = SOURCES;
            sources[index] = &changed;
            reject(&format!("{label}:{id}"), oracle, &table, sources, WIRING);
        }
        let changed = SOURCES[index].replace(
            &format!("check(\"{id}\").await;"),
            "check(\"unknown/cell\").await;",
        );
        let mut sources = SOURCES;
        sources[index] = &changed;
        reject(&format!("wrong-cell:{id}"), oracle, &table, sources, WIRING);
    }
    for index in 0..3 {
        let declaration = "#[tokio::test(start_paused = true)]\nasync fn known_differences_remain_exact_and_ignored() {";
        for (label, replacement) in [
            (
                "missing-replay-guard",
                "async fn known_differences_remain_exact_and_ignored() {",
            ),
            (
                "disabled-replay-guard",
                "#[tokio::test(start_paused = true)]\n#[cfg(any())]\nasync fn known_differences_remain_exact_and_ignored() {",
            ),
        ] {
            assert_eq!(SOURCES[index].matches(declaration).count(), 1);
            let changed = SOURCES[index].replace(declaration, replacement);
            let mut sources = SOURCES;
            sources[index] = &changed;
            reject(&format!("{label}:{index}"), oracle, &table, sources, WIRING);
        }
    }
    for index in 0..2 {
        let changed = WIRING[index].replace(
            "mod network_timing_tests;",
            "mod disconnected_network_timing_tests;",
        );
        let mut wiring = WIRING;
        wiring[index] = &changed;
        reject(
            &format!("missing-wiring:{index}"),
            oracle,
            &table,
            SOURCES,
            wiring,
        );
        let changed = WIRING[index].replace(
            "#[cfg(test)]\n#[path = \"../../../tests/common/network_timing_",
            "#[cfg(any())]\n\n// cfg still applies\n#[cfg(test)]\n#[path = \"../../../tests/common/network_timing_",
        );
        let mut wiring = WIRING;
        wiring[index] = &changed;
        reject(
            &format!("disabled-wiring:{index}"),
            oracle,
            &table,
            SOURCES,
            wiring,
        );
    }
    for row in table["cells"].as_array().unwrap().iter().filter(|row| {
        row["id"].as_str().unwrap().starts_with("mcp_")
            || row["id"].as_str().unwrap().starts_with("hooks/")
    }) {
        for field in ["ts", "rust", "test", "category"] {
            let mut changed = table.clone();
            let entry = changed["cells"]
                .as_array_mut()
                .unwrap()
                .iter_mut()
                .find(|e| e["id"] == row["id"])
                .unwrap();
            entry[field] = match field {
                "ts" => Value::Null,
                "rust" => row["ts"].clone(),
                _ => Value::String("unknown".into()),
            };
            reject(
                &format!("difference-{field}:{}", row["id"]),
                oracle,
                &changed,
                SOURCES,
                WIRING,
            );
        }
    }
    let mut changed = oracle.clone();
    changed["cases"].as_array_mut().unwrap().pop();
    reject("missing-oracle-cell", &changed, &table, SOURCES, WIRING);
    let mut changed = table.clone();
    changed["cells"].as_array_mut().unwrap().pop();
    reject("missing-difference", oracle, &changed, SOURCES, WIRING);
    println!("REGISTRY_NEGATIVE {count} controls PASS");
}
