#[path = "common/baseline.rs"]
mod baseline;

use pilotdeck_gateway::storage::{FileRunRegistry, read_transcript};
use serde_json::{Value, json};
use std::path::PathBuf;

struct Scratch(PathBuf);
impl Scratch {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!("pdrs-502-syntax-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&root).unwrap();
        Self(root)
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).unwrap();
    }
}
fn cases() -> Vec<Value> {
    let raw = include_str!("fixtures/diff-backfill/oracle.json");
    baseline::oracle(raw);
    let oracle: Value = serde_json::from_str(raw).unwrap();
    let cases = oracle["syntaxCases"].as_array().unwrap();
    assert_eq!(cases.len(), 8);
    cases.clone()
}

#[test]
fn transcript_syntax_diagnostics_match_real_ts() {
    let scratch = Scratch::new();
    let file = scratch.0.join("transcript.jsonl");
    for case in cases() {
        std::fs::write(&file, case["input"].as_str().unwrap()).unwrap();
        let actual = read_transcript(&file).unwrap();
        assert!(actual.entries.is_empty());
        let diagnostics: Vec<_> = actual
            .diagnostics
            .iter()
            .map(|diagnostic| {
                json!({
                    "code": diagnostic.code, "severity": diagnostic.severity,
                    "message": diagnostic.message, "line": diagnostic.line,
                })
            })
            .collect();
        assert_eq!(json!(diagnostics), case["diagnostics"], "{case}");
    }
}

#[test]
fn run_registry_syntax_errors_match_real_ts() {
    let scratch = Scratch::new();
    let file = scratch.0.join("runs.json");
    for case in cases() {
        std::fs::write(&file, case["input"].as_str().unwrap()).unwrap();
        let error = FileRunRegistry::open(&file)
            .err()
            .expect("malformed registry");
        assert_eq!(
            error.to_string(),
            case["registryError"].as_str().unwrap(),
            "{case}"
        );
    }
}
