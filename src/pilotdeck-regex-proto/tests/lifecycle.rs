//! #1265 S4: the lifecycle matrix (state × event × timing).
//!
//! Node v22.22.0 / ICU 78.2 anchors were measured in
//! `_tmp/20261107-issue-1265/s4-probe.log` (task evidence directory); the
//! matrix itself lives in the same directory's plan.md. Rows the prototype
//! cannot reach are recorded there as 不适用 with their reasons, not here.

#![cfg(feature = "regex-proto")]

use pilotdeck_regex_proto::{
    EngineLimits, ExecOutcome, MatchBudget, MatchError, MatchResult, RegexEngine,
};

fn engine() -> RegexEngine {
    RegexEngine::new(EngineLimits::new()).expect("#1265 engine init")
}

/// The full-match result for `pattern` on a subject it matches completely.
fn full_match(length: usize) -> ExecOutcome {
    ExecOutcome::Matched(MatchResult {
        index: 0,
        length,
        groups: Some(vec![]),
    })
}

fn units(text: &str) -> Vec<u16> {
    text.encode_utf16().collect()
}

fn compile(
    engine: &RegexEngine,
    pattern: &str,
    flags: &str,
) -> pilotdeck_regex_proto::CompiledPattern {
    engine
        .compile(&units(pattern), flags)
        .expect("#1265 compile")
}

/// #685 report anchor: QuickJS `reuse_after_interrupt=Ok(true)` — an
/// interrupted engine keeps serving later requests.
#[test]
fn reuse_after_interrupt_serves_later_requests() {
    let engine = engine();
    let pattern = compile(&engine, "^(a+)+(?=!)$", "");
    let doom = units(&format!("{}!", "a".repeat(18)));

    let mut tiny = MatchBudget::new(1);
    assert_eq!(
        engine.exec(&pattern, &doom, &mut tiny),
        Err(MatchError::Interrupted),
        "#1265 the request must exhaust first"
    );

    // The same engine and pattern, now with a budget that suffices: the full
    // catastrophic match runs to its own verdict.
    let mut plenty = MatchBudget::new(u64::MAX);
    assert_eq!(
        engine.exec(&pattern, &doom, &mut plenty),
        Ok(ExecOutcome::NoMatch),
        "#1265 the engine must serve the interrupted request again"
    );

    // And an ordinary match afterwards is untouched by the earlier interrupt.
    let plain = compile(&engine, "^a+$", "");
    let mut budget = MatchBudget::new(u64::MAX);
    let outcome = engine
        .exec(&plain, &units("aaa"), &mut budget)
        .expect("#1265 post-interrupt exec must complete");
    assert_eq!(
        outcome,
        full_match(3),
        "#1265 post-interrupt matches must be ordinary"
    );
}

/// Node measurement (s4-probe.log): without flags `re.exec`/`re.test` are
/// stateless; with `g` the *same RegExp object* consumes `lastIndex` across
/// calls (`g-exec-3-null`, `g-test-3 false`), while a fresh object restarts at
/// 0 (`fresh-g-exec-indices [0,0]`). The prototype's bridge resets
/// `re.lastIndex = 0` before every exec, so every call is a full exec from the
/// start of the subject — the `g` flag is accepted but carries no cross-call
/// state. `lastIndex` itself is deliberately not part of the API surface.
#[test]
fn exec_is_stateless_per_call_regardless_of_the_global_flag() {
    let engine = engine();
    for flags in ["", "g"] {
        let pattern = compile(&engine, "a", flags);
        let mut first = MatchBudget::new(u64::MAX);
        let mut second = MatchBudget::new(u64::MAX);
        let one = engine
            .exec(&pattern, &units("aa"), &mut first)
            .expect("#1265 first exec must complete");
        let two = engine
            .exec(&pattern, &units("aa"), &mut second)
            .expect("#1265 second exec must complete");
        // Both calls see the same result, unlike Node's same-object `g`
        // consumption where the third call returns null.
        assert_eq!(one, two, "#1265 exec must stay stateless per call");
        assert_eq!(one, full_match(1));
    }
}

/// One QuickJS runtime per engine, one engine per thread (the documented
/// single-threaded contract): budgets and interrupt state never cross engines.
#[test]
fn engines_do_not_share_budget_or_interrupt_state() {
    let left = engine();
    let right = engine();
    let doom = units(&format!("{}!", "a".repeat(18)));
    let left_pattern = compile(&left, "^(a+)+(?=!)$", "");
    let right_pattern = compile(&right, "^(a+)+(?=!)$", "");

    let mut tiny = MatchBudget::new(1);
    assert_eq!(
        left.exec(&left_pattern, &doom, &mut tiny),
        Err(MatchError::Interrupted),
        "#1265 the left engine must exhaust"
    );

    // The right engine is fully unaffected by the left one's interruption.
    let mut plenty = MatchBudget::new(u64::MAX);
    assert_eq!(
        right.exec(&right_pattern, &doom, &mut plenty),
        Ok(ExecOutcome::NoMatch),
        "#1265 a second engine must not inherit the first engine's interrupt"
    );

    // And the left engine recovers on its own with a fresh budget.
    let mut fresh = MatchBudget::new(u64::MAX);
    assert_eq!(
        left.exec(&left_pattern, &units("!"), &mut fresh),
        Ok(ExecOutcome::NoMatch),
        "#1265 the left engine must recover with a fresh budget"
    );
}

/// Pattern ids are per-engine (the bootstrap's `next` counter restarts in
/// every context), so a pattern from another engine must be rejected instead
/// of silently running this engine's same-id pattern: `left`'s id 0 must not
/// resolve to `right`'s id 0 and return `right`'s verdict for `left`'s
/// pattern.
#[test]
fn exec_rejects_a_pattern_compiled_by_another_engine() {
    let left = engine();
    let right = engine();
    let pattern = compile(&left, "^a+$", "u");
    let error = right
        .exec(&pattern, &units("aaa"), &mut MatchBudget::new(u64::MAX))
        .expect_err("#1265 a cross-engine exec must be rejected");
    assert!(
        matches!(error, MatchError::Engine(ref message) if message.contains("another engine")),
        "#1265 unexpected cross-engine error: {error:?}"
    );
    // Both engines keep serving their own patterns afterwards.
    let mut budget = MatchBudget::new(u64::MAX);
    let outcome = left
        .exec(&pattern, &units("aaa"), &mut budget)
        .expect("#1265 the owning engine must still serve");
    assert_eq!(outcome, full_match(3));
}

/// Patterns hold their engine alive: dropping the last engine handle while a
/// pattern is still alive must not abort when the pattern itself drops —
/// `Arc<EngineInner>` keeps the QuickJS runtime alive through the pattern's
/// own release call (the reason the engine avoids `Persistent<T>`, whose
/// cross-runtime drop can abort).
#[test]
fn dropping_the_engine_keeps_the_runtime_alive_for_pattern_drops() {
    let origin = engine();
    let pattern = compile(&origin, "^a+$", "u");
    drop(origin);
    // The pattern's Drop now releases its JS-side cache entry on the runtime
    // the Arc kept alive; this must be a clean release, not an abort.
    drop(pattern);

    // A fresh engine on the same thread still works afterwards.
    let restart = engine();
    let pattern = compile(&restart, "^a+$", "u");
    let mut budget = MatchBudget::new(u64::MAX);
    let outcome = restart
        .exec(&pattern, &units("aa"), &mut budget)
        .expect("#1265 the fresh engine must serve");
    assert_eq!(outcome, full_match(2));
}
