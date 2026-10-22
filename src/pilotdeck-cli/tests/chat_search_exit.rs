//! The oracle comes from the unmodified TS CLI action and native JSONL provider.
use std::{fs, process::Command};

fn parity(source: &str, errors_only: bool) {
    let oracle: serde_json::Value = serde_json::from_str(source).unwrap();
    assert_eq!(
        oracle["provenance"]["sourceCommit"],
        "cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85"
    );
    let root = std::env::temp_dir().join(format!(
        "pdrs-chat-exit-{}-{errors_only}-{}",
        std::process::id(),
        source.len()
    ));
    fs::create_dir_all(root.join("project")).unwrap();
    for (name, text) in oracle["files"].as_object().unwrap() {
        let path = root.join(name);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(
            path,
            text.as_str()
                .unwrap()
                .replace("{runtime}", &root.to_string_lossy()),
        )
        .unwrap();
    }
    let mut failures = Vec::new();
    for case in oracle["cases"].as_array().unwrap() {
        let id = case["id"].as_str().unwrap();
        let pre_storage = matches!(id, "usage" | "unknown" | "empty" | "flags-only");
        if pre_storage != errors_only {
            continue;
        }
        let args: Vec<_> = case["argv"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        let output = Command::new(env!("CARGO_BIN_EXE_pilotdeck"))
            .arg("chat")
            .args(args)
            .env_clear()
            .env("HOME", root.join("home"))
            .env("PILOT_HOME", root.join("pilot"))
            .env("TZ", "UTC")
            .env("LANG", "C")
            .env("LC_ALL", "C")
            .env("NO_COLOR", "1")
            .current_dir(root.join("project"))
            .output()
            .unwrap();
        let expected = |key: &str| {
            case[key]
                .as_str()
                .unwrap()
                .replace("{runtime}", &root.to_string_lossy())
                .into_bytes()
        };
        if output.status.code() != case["exit"].as_i64().map(|n| n as i32)
            || output.stdout != expected("stdout")
            || output.stderr != expected("stderr")
        {
            failures.push(format!(
                "{id}: exit={:?} stdout={:?} stderr={:?}",
                output.status.code(),
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            ));
        }
    }
    fs::remove_dir_all(&root).unwrap();
    assert!(
        failures.is_empty(),
        "TS CLI parity mismatches:\n{}",
        failures.join("\n")
    );
}

#[test]
fn chat_search_usage_and_keyword_errors_match_ts() {
    parity(include_str!("fixtures/chat-search/oracle.json"), true);
}

#[test]
fn chat_search_storage_and_format_match_ts() {
    parity(include_str!("fixtures/chat-search/oracle.json"), false);
}

#[test]
fn chat_search_utf16_and_raw_storage_edges_match_ts() {
    parity(include_str!("fixtures/chat-search/edges.json"), false);
}
