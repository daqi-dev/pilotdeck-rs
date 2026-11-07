//! #1265 S2: replay of the 108-case regress oracle.
//!
//! `src/pilotdeck-gateway/tests/fixtures/output-schema/regress-oracle.json`
//! comes from WIP commit `2c2e4f87` (branch `codex/regress-pr1-output-schema`);
//! it was regenerated from the fixed TS baseline with Node v22.22.0 and
//! matched the committed bytes sha256-for-sha256 before these tests were
//! written. Ajv compiles both `pattern` and `patternProperties` keys with
//! the `u` flag (`regexCompilation` in the oracle), so the replay compiles
//! every pattern with `"u"` too. `errorText` is Ajv's message formatting and
//! is deliberately not compared: this crate replays regex semantics only.

#![cfg(feature = "regex-proto")]

use pilotdeck_protocol::{JsValue, parse_json};
use pilotdeck_regex_proto::{CompileError, EngineLimits, ExecOutcome, MatchBudget, RegexEngine};

const ORACLE: &str =
    include_str!("../../pilotdeck-gateway/tests/fixtures/output-schema/regress-oracle.json");

/// The only verdict divergences between QuickJS-NG 0.16.2 (the engine behind
/// rquickjs 0.14.0) and the Node v22.22.0 / V8 oracle across all 108 cases.
///
/// V8 rejects a `u`-flag pattern that contains a *raw* lone surrogate code
/// unit with `URIError: URI malformed` (Ajv records it as a compileError),
/// while the escaped `^\uD800$` form is accepted. QuickJS accepts both, so the
/// two `key` cases — whose patternProperties key carries the raw unit —
/// compile on the engine where the SDK failed. The expected verdict pins the
/// engine's own behaviour for those patterns (measured on the full replay
/// run, `_tmp/20261107-issue-1265/s2-engine-full-dump.txt` in the task
/// evidence directory), so this list fails loudly if either side changes.
const KNOWN_COMPILE_DIVERGENCES: &[(&str, &str)] = &[
    ("utf16/raw-lone-pattern/key/positive", "matched"),
    ("utf16/raw-lone-pattern/key/negative", "no-match"),
];

#[test]
fn replays_the_108_case_regress_oracle() {
    let oracle = parse_json(ORACLE).expect("#1265 parse regress-oracle.json");
    let cases = match oracle.get("cases").expect("#1265 cases") {
        JsValue::Array(cases) => cases,
        other => panic!("#1265 cases must be an array: {other:?}"),
    };
    assert_eq!(cases.len(), 108, "#1265 oracle case count");
    // The replay's flag choice is the oracle's own recorded evidence: both
    // the value pattern and the patternProperties key compile with "u".
    let compilation = oracle
        .get("regexCompilation")
        .expect("#1265 regexCompilation");
    let JsValue::Array(probes) = compilation else {
        panic!("#1265 regexCompilation must be an array")
    };
    assert_eq!(probes.len(), 2, "#1265 two compilation probes");
    for (probe, pattern) in probes.iter().zip(["^sdk_flag_probe$", "^sdk_key_probe$"]) {
        assert!(
            probe
                .get("pattern")
                .and_then(JsValue::as_string)
                .is_some_and(|p| p.equals_str(pattern)),
            "#1265 probe pattern {probe:?}"
        );
        assert!(
            probe
                .get("flags")
                .and_then(JsValue::as_string)
                .is_some_and(|f| f.equals_str("u")),
            "#1265 probe flags {probe:?}"
        );
    }
    assert!(
        oracle
            .get("node")
            .and_then(JsValue::as_string)
            .is_some_and(|n| n.equals_str("v22.22.0")),
        "#1265 oracle node version"
    );

    let engine = RegexEngine::new(EngineLimits::new()).expect("#1265 engine init");
    let mut compiled_count = 0;
    let mut compile_errors = 0;
    let mut verdicts = 0;
    let mut known_divergences = 0;
    // Every divergence is collected and asserted once at the end, so one run
    // reports the complete picture instead of stopping at the first case.
    let mut divergences: Vec<String> = Vec::new();
    for case in cases {
        let id = case
            .get("id")
            .and_then(JsValue::as_string)
            .expect("#1265 case id");
        let id = String::from_utf16_lossy(id.units());
        let schema = case.get("schema").expect("#1265 schema");
        let pattern: Vec<u16> = if let Some(pattern) =
            schema.get("pattern").and_then(JsValue::as_string)
        {
            pattern.units().to_vec()
        } else {
            let properties = schema
                .get("patternProperties")
                .and_then(JsValue::as_object)
                .unwrap_or_else(|| panic!("#1265 {id:?}: neither pattern nor patternProperties"));
            assert_eq!(
                properties.entries().len(),
                1,
                "#1265 {id:?}: one key expected"
            );
            properties.entries()[0].0.units().to_vec()
        };
        let data = case.get("data").expect("#1265 data");
        let subject: Vec<u16> = match data {
            JsValue::String(subject) => subject.units().to_vec(),
            JsValue::Object(map) => {
                assert_eq!(
                    map.entries().len(),
                    1,
                    "#1265 {id:?}: one property expected"
                );
                map.entries()[0].0.units().to_vec()
            }
            other => panic!("#1265 {id:?}: unexpected data shape {other:?}"),
        };
        let expected_compile_error = case.get("compileError").is_some();
        let known = KNOWN_COMPILE_DIVERGENCES
            .iter()
            .find(|(known, _)| *known == id);
        if std::env::var_os("PDRS_1265_DUMP").is_some() {
            let verdict = match engine.compile(&pattern, "u") {
                Err(error) => format!("compile-error {error:?}"),
                Ok(compiled) => {
                    let mut budget = MatchBudget::new(u64::MAX);
                    match engine.exec(&compiled, &subject, &mut budget) {
                        Ok(ExecOutcome::Matched(_)) => "matched".to_owned(),
                        Ok(ExecOutcome::NoMatch) => "no-match".to_owned(),
                        Err(error) => format!("exec-error {error:?}"),
                    }
                }
            };
            eprintln!(
                "#1265 case {id} engine={verdict} sdk_compile_error={expected_compile_error}"
            );
        }
        match engine.compile(&pattern, "u") {
            Err(CompileError::Syntax(_)) => {
                if !expected_compile_error {
                    divergences.push(format!("{id}: engine rejected a pattern the SDK accepted"));
                } else if known.is_some() {
                    divergences.push(format!(
                        "{id}: known-divergence pattern no longer compiles on the engine"
                    ));
                } else {
                    compile_errors += 1;
                }
            }
            Err(other) => divergences.push(format!("{id}: unexpected compile error {other:?}")),
            Ok(compiled) => {
                if let Some((_, expected_verdict)) = known {
                    // Known divergence: the SDK rejected the raw-unit pattern
                    // (URIError) where QuickJS compiles it. Pin the engine's
                    // verdict so the documented difference stays measurable.
                    let mut budget = MatchBudget::new(u64::MAX);
                    let actual = match engine
                        .exec(&compiled, &subject, &mut budget)
                        .expect("#1265 {id}: exec must complete")
                    {
                        ExecOutcome::Matched(_) => "matched",
                        ExecOutcome::NoMatch => "no-match",
                    };
                    assert_eq!(
                        actual, *expected_verdict,
                        "#1265 {id}: known-divergence verdict changed"
                    );
                    known_divergences += 1;
                    continue;
                }
                if expected_compile_error {
                    divergences.push(format!("{id}: engine accepted a pattern the SDK rejected"));
                    continue;
                }
                compiled_count += 1;
                let mut budget = MatchBudget::new(u64::MAX);
                let outcome = engine
                    .exec(&compiled, &subject, &mut budget)
                    .expect("#1265 {id}: exec must complete");
                let valid = case
                    .get("valid")
                    .and_then(JsValue::as_bool)
                    .expect("#1265 valid flag");
                match outcome {
                    ExecOutcome::Matched(_) => {
                        if !valid {
                            divergences.push(format!("{id}: matched but oracle says invalid"));
                        }
                        verdicts += 1;
                    }
                    ExecOutcome::NoMatch => {
                        if valid {
                            divergences.push(format!("{id}: no match but oracle says valid"));
                        }
                        verdicts += 1;
                    }
                }
            }
        }
    }
    assert!(
        divergences.is_empty(),
        "#1265 oracle divergences ({}): {:#?}",
        divergences.len(),
        divergences
    );
    // The oracle's own composition: 14 compile errors and 94 validation
    // results, of which 12 rejections and all 94 verdicts agree with the
    // engine; the remaining 2 compile errors are the documented divergence.
    assert_eq!(known_divergences, 2, "#1265 known-divergence count");
    assert_eq!(compile_errors, 12, "#1265 compile-error count");
    assert_eq!(compiled_count, 94, "#1265 compiled count");
    assert_eq!(verdicts, 94, "#1265 verdict count");
}
