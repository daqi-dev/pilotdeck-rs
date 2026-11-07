//! #1265 S3: match-budget semantics (three-state, charging, per-request reset).
//!
//! The budget contract under test comes from the #685 report's acceptance
//! checklist: exhaustion must be an *explicit* error distinguishable from both
//! completed verdicts (match and no-match), never a silent downgrade to
//! "false"/no-match. The catastrophic pattern below is the report's own probe
//! shape (`^(a+)+(?=!)$` on a run of `a`s followed by `!`).

#![cfg(feature = "regex-proto")]

use pilotdeck_regex_proto::{
    EngineLimits, ExecOutcome, MatchBudget, MatchError, MatchResult, RegexEngine,
};

fn engine() -> RegexEngine {
    RegexEngine::new(EngineLimits::new()).expect("#1265 engine init")
}

/// The #685 catastrophic-backtracking probe: never matches, but the failed
/// lookahead drives exponential backtracking before the engine gives up.
fn catastrophic(engine: &RegexEngine) -> pilotdeck_regex_proto::CompiledPattern {
    engine
        .compile(&"^(a+)+(?=!)$".encode_utf16().collect::<Vec<_>>(), "")
        .expect("#1265 compile catastrophic probe")
}

fn units(text: &str) -> Vec<u16> {
    text.encode_utf16().collect()
}

#[test]
fn budget_states_are_independently_distinguishable() {
    let engine = engine();
    let pattern = catastrophic(&engine);
    let doom = units(&format!("{}!", "a".repeat(18)));

    // State 1 — normal completion with a match (a *different*, tractable
    // pattern): a plain anchored repetition matches quickly.
    let plain = engine
        .compile(&units("^(a+)$"), "")
        .expect("#1265 compile plain");
    let mut big = MatchBudget::new(u64::MAX);
    assert_eq!(
        engine.exec(&plain, &units("aaaa"), &mut big),
        Ok(ExecOutcome::Matched(MatchResult {
            index: 0,
            length: 4,
            groups: Some(vec![Some((0, 4))]),
        })),
        "#1265 budget state must stay a match under a large budget"
    );

    // State 2 — normal completion without a match: the same catastrophic
    // pattern against a short subject finishes on its own with NoMatch.
    let mut quick = MatchBudget::new(u64::MAX);
    assert_eq!(
        engine.exec(&pattern, &units("!"), &mut quick),
        Ok(ExecOutcome::NoMatch),
        "#1265 budget state must stay no-match when the engine finishes"
    );

    // State 3 — exhaustion on the same pattern/subject pair that would
    // otherwise finish: an explicit error, distinct from both Ok verdicts.
    let mut tiny = MatchBudget::new(1);
    assert_eq!(
        engine.exec(&pattern, &doom, &mut tiny),
        Err(MatchError::Interrupted),
        "#1265 budget exhaustion must be an explicit interrupt, not a verdict"
    );
    assert_ne!(
        engine.exec(&pattern, &doom, &mut tiny),
        Ok(ExecOutcome::NoMatch),
        "#1265 exhaustion must never collapse into the no-match state"
    );
    // The very same request with a large budget completes (with no match).
    let mut plenty = MatchBudget::new(u64::MAX);
    let granted = plenty.remaining_polls();
    assert_eq!(
        engine.exec(&pattern, &doom, &mut plenty),
        Ok(ExecOutcome::NoMatch),
        "#1265 a large budget must complete the same request"
    );
    // Compare against the granted allowance, not `u64::MAX`: `new` saturates
    // to `i64::MAX`, so `< u64::MAX` holds even when nothing was charged.
    assert!(
        plenty.remaining_polls() < granted,
        "#1265 polls must be charged to the completing request"
    );
}

#[test]
fn zero_budget_interrupts_on_the_first_poll() {
    // `new(0)` stores 0, so the handler's first poll reads `previous <= 0`
    // and interrupts: a zero budget must never run untracked.
    let engine = engine();
    let pattern = catastrophic(&engine);
    let doom = units(&format!("{}!", "a".repeat(18)));
    let mut zero = MatchBudget::new(0);
    assert_eq!(
        engine.exec(&pattern, &doom, &mut zero),
        Err(MatchError::Interrupted),
        "#1265 a zero budget must interrupt on its first poll"
    );
    assert_eq!(
        zero.remaining_polls(),
        0,
        "#1265 a zero budget reports nothing remaining"
    );
}

#[test]
fn exhausted_budget_reports_zero_remaining() {
    let engine = engine();
    let pattern = catastrophic(&engine);
    let doom = units(&format!("{}!", "a".repeat(18)));
    let mut budget = MatchBudget::new(1);
    assert_eq!(
        engine.exec(&pattern, &doom, &mut budget),
        Err(MatchError::Interrupted),
        "#1265 exhaustion must interrupt"
    );
    assert_eq!(
        budget.remaining_polls(),
        0,
        "#1265 an exhausted budget must report zero polls remaining"
    );
}

#[test]
fn budget_spans_every_exec_of_one_request() {
    let engine = engine();
    let pattern = catastrophic(&engine);
    // One budget shared by consecutive execs: the second starts from the
    // first's remainder, so the total is what the caller granted the request.
    let mut budget = MatchBudget::new(u64::MAX);
    let granted = budget.remaining_polls();
    let fast = units(&format!("{}!", "a".repeat(4)));
    assert_eq!(
        engine.exec(&pattern, &fast, &mut budget),
        Ok(ExecOutcome::NoMatch),
        "#1265 first fast exec must complete"
    );
    let after_first = budget.remaining_polls();
    // Against the granted allowance (not `u64::MAX`, which saturates away).
    assert!(
        after_first < granted,
        "#1265 the first exec must charge polls"
    );
    let slow = units(&format!("{}!", "a".repeat(18)));
    assert_eq!(
        engine.exec(&pattern, &slow, &mut budget),
        Ok(ExecOutcome::NoMatch),
        "#1265 second exec on the shared budget must still complete"
    );
    assert!(
        budget.remaining_polls() < after_first,
        "#1265 the shared budget must keep charging across the request's execs"
    );
}

#[test]
fn fresh_budget_does_not_inherit_previous_exhaustion() {
    let engine = engine();
    let pattern = catastrophic(&engine);
    let doom = units(&format!("{}!", "a".repeat(18)));

    // Exhaust one request's budget completely.
    let mut spent = MatchBudget::new(1);
    assert_eq!(
        engine.exec(&pattern, &doom, &mut spent),
        Err(MatchError::Interrupted),
        "#1265 the first request must exhaust"
    );

    // A new request with a fresh budget on the same engine and pattern must
    // not inherit the previous request's exhaustion (V8 comparison: every
    // `re.test()` starts with no hidden counter; the #685 report also records
    // QuickJS `reuse_after_interrupt=Ok(true)`).
    let mut fresh = MatchBudget::new(u64::MAX);
    assert_eq!(
        engine.exec(&pattern, &units("!"), &mut fresh),
        Ok(ExecOutcome::NoMatch),
        "#1265 a fresh budget must not inherit the previous request's exhaustion"
    );

    // Compilation after an interruption also stays unaffected.
    engine
        .compile(&units("^z$"), "u")
        .expect("#1265 compile after an interrupted request");
}
