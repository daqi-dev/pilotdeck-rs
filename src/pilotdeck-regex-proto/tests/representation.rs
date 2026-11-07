//! #1265 S2: UTF-16 representation tests.
//!
//! Every expected value below was first measured on the fixed Node
//! v22.22.0 / ICU 78.2 (see `_tmp/20261107-issue-1265/s2-counterexample-probe.log`
//! in the task evidence directory); these tests pin that measured behaviour,
//! not a hand-written interpretation of ECMAScript.

#![cfg(feature = "regex-proto")]

use pilotdeck_regex_proto::{EngineLimits, ExecOutcome, MatchBudget, MatchResult, RegexEngine};

fn engine() -> RegexEngine {
    RegexEngine::new(EngineLimits::new()).expect("#1265 engine init")
}

fn compile(
    engine: &RegexEngine,
    pattern: &str,
    flags: &str,
) -> pilotdeck_regex_proto::CompiledPattern {
    engine
        .compile(&pattern.encode_utf16().collect::<Vec<_>>(), flags)
        .expect("#1265 compile")
}

/// Match and return the result; asserts a completed (non-interrupted) match.
fn matched_units(engine: &RegexEngine, pattern: &str, flags: &str, subject: &[u16]) -> MatchResult {
    let compiled = compile(engine, pattern, flags);
    let mut budget = MatchBudget::new(u64::MAX);
    match engine
        .exec(&compiled, subject, &mut budget)
        .expect("#1265 exec must complete")
    {
        ExecOutcome::Matched(result) => result,
        ExecOutcome::NoMatch => panic!("#1265 expected a match for {pattern:?}"),
    }
}

/// Like [`matched_units`] with a Rust string subject (no lone surrogates).
fn matched(engine: &RegexEngine, pattern: &str, flags: &str, subject: &str) -> MatchResult {
    matched_units(
        engine,
        pattern,
        flags,
        &subject.encode_utf16().collect::<Vec<_>>(),
    )
}

fn no_match_units(engine: &RegexEngine, pattern: &str, flags: &str, subject: &[u16]) {
    let compiled = compile(engine, pattern, flags);
    let mut budget = MatchBudget::new(u64::MAX);
    assert_eq!(
        engine.exec(&compiled, subject, &mut budget),
        Ok(ExecOutcome::NoMatch),
        "#1265 expected no match for {pattern:?}"
    );
}

fn no_match(engine: &RegexEngine, pattern: &str, flags: &str, subject: &str) {
    no_match_units(
        engine,
        pattern,
        flags,
        &subject.encode_utf16().collect::<Vec<_>>(),
    )
}

fn groups(result: &MatchResult) -> Vec<Option<(usize, usize)>> {
    result
        .groups
        .clone()
        .expect("#1265 indices must be available (d-flag twin)")
}

#[test]
fn dot_without_u_matches_a_single_code_unit_not_a_code_point() {
    // Node: /^.$/.exec("😀") === null — the pair spans two units.
    no_match(&engine(), "^.$", "", "\u{1F600}");
    // Node: /^.$/u matches [0,2).
    assert_eq!(
        matched(&engine(), "^.$", "u", "\u{1F600}"),
        MatchResult {
            index: 0,
            length: 2,
            groups: Some(vec![]),
        }
    );
    // Node: /./ on "😀" captures the first (high) surrogate, range [0,1).
    assert_eq!(
        matched(&engine(), ".", "", "\u{1F600}"),
        MatchResult {
            index: 0,
            length: 1,
            groups: Some(vec![]),
        }
    );
}

#[test]
fn lone_surrogates_round_trip_in_pattern_and_subject() {
    let engine = engine();
    // Node: /^\uD800$/ matches the lone high surrogate; both sides carry the
    // exact code unit, never a replacement character.
    assert_eq!(
        matched_units(&engine, "^\\uD800$", "", &[0xd800]),
        MatchResult {
            index: 0,
            length: 1,
            groups: Some(vec![]),
        }
    );
    assert_eq!(
        matched_units(&engine, "^\\uDFFF$", "", &[0xdfff]),
        MatchResult {
            index: 0,
            length: 1,
            groups: Some(vec![]),
        }
    );
    // The pattern itself carries the raw unit (no \u escape): pattern units
    // [0xd800, 0x64, 0x100... ] — "^" + 0xd800 + "$" matched against the same
    // lone unit.
    let raw: Vec<u16> = vec![0x5e, 0xd800, 0x24];
    let subject: Vec<u16> = vec![0xd800];
    let compiled = engine
        .compile(&raw, "")
        .expect("#1265 raw lone-surrogate pattern compiles");
    let mut budget = MatchBudget::new(u64::MAX);
    assert_eq!(
        engine.exec(&compiled, &subject, &mut budget),
        Ok(ExecOutcome::Matched(MatchResult {
            index: 0,
            length: 1,
            groups: Some(vec![]),
        })),
        "#1265 raw lone-surrogate pattern must match the lone unit"
    );
}

#[test]
fn match_can_start_inside_a_surrogate_pair_at_code_unit_offsets() {
    // Node: /\uDE00/.exec("😀") → index 1, length 1 — a nonzero start in the
    // middle of the pair, and all offsets are UTF-16 code units.
    assert_eq!(
        matched(&engine(), "\\uDE00", "", "\u{1F600}"),
        MatchResult {
            index: 1,
            length: 1,
            groups: Some(vec![]),
        }
    );
}

#[test]
fn pattern_containing_a_surrogate_pair_matches_under_u() {
    // Node: /^😀$/u on "😀" matches [0,2).
    assert_eq!(
        matched(&engine(), "^\\uD83D\\uDE00$", "u", "\u{1F600}"),
        MatchResult {
            index: 0,
            length: 2,
            groups: Some(vec![]),
        }
    );
    // Node also compiles a lone-surrogate escape under /u (source unchanged).
    let engine = engine();
    let compiled = engine
        .compile(&"^\\uD800$".encode_utf16().collect::<Vec<_>>(), "u")
        .expect("#1265 V8 compiles the lone-high escape under u");
    let mut budget = MatchBudget::new(u64::MAX);
    assert_eq!(
        engine.exec(&compiled, &[0xd800], &mut budget),
        Ok(ExecOutcome::Matched(MatchResult {
            index: 0,
            length: 1,
            groups: Some(vec![]),
        })),
        "#1265 lone surrogate subject must match under u"
    );
}

#[test]
fn lookbehind_reverse_captures_match_the_node_shape() {
    // Node: /(?<=([ab]+)([bc]+))$/.exec("abc") → index 3, length 0,
    // groups ["a" [0,1), "bc" [1,3)) — the exact counterexample that broke
    // fancy-regex ([[],[97],[98],[99]]-family divergences) and Onig.
    let result = matched(&engine(), "(?<=([ab]+)([bc]+))$", "", "abc");
    assert_eq!(result.index, 3, "#1265 full-match index");
    assert_eq!(result.length, 0, "#1265 full-match length");
    assert_eq!(groups(&result), vec![Some((0, 1)), Some((1, 3))]);
}

#[test]
fn unmatched_backreference_group_is_reported_as_none() {
    // Node: /^(a|(b))\2c$/.exec("ac") → match, group 1 "a" [0,1),
    // group 2 null (unset backreference matches empty).
    let result = matched(&engine(), "^(a|(b))\\2c$", "", "ac");
    assert_eq!(result.index, 0);
    assert_eq!(result.length, 2);
    assert_eq!(groups(&result), vec![Some((0, 1)), None]);
}

#[test]
fn character_classes_are_ascii_and_js_space_pins() {
    let engine = engine();
    // Node: /^\w+$/ on "é" → null; /^\d+$/ on "٣" (U+0663) → null.
    no_match(&engine, "^\\w+$", "", "é");
    no_match(&engine, "^\\d+$", "", "٣");
    // Node: /^\s$/ matches U+FEFF (BOM) and U+2028 (LINE SEPARATOR).
    assert_eq!(
        matched(&engine, "^\\s$", "", "\u{FEFF}").length,
        1,
        "#1265 BOM is JS whitespace"
    );
    assert_eq!(
        matched(&engine, "^\\s$", "", "\u{2028}").length,
        1,
        "#1265 U+2028 is JS whitespace"
    );
    // Node: /^.$/ on "\r" → null — dot excludes line terminators.
    no_match(&engine, "^.$", "", "\r");
    // Node: /^\q$/ (Annex B identity escape, no u) matches "q".
    assert_eq!(
        matched(&engine, "^\\q$", "", "q").length,
        1,
        "#1265 identity escape"
    );
}

#[test]
fn negated_empty_class_matches_any_unit_including_lone_surrogates() {
    // From the u-not-v oracle family: [^] under /u is "any single unit";
    // Node matches a lone surrogate but not the empty string.
    let engine = engine();
    assert_eq!(
        matched_units(&engine, "[^]", "u", &[0xd800]).length,
        1,
        "#1265 [^] matches the lone surrogate"
    );
    no_match(&engine, "[^]", "u", "");
}
