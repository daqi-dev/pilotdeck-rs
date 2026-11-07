//! #1265 S3: compile-time resource limits.
//!
//! Every rejected shape below was first measured on this engine
//! (`_tmp/20261107-issue-1265/s3-resource-probe.log` in the task evidence
//! directory); the expected variants are those measurements, pinned. The
//! engine's own classifications:
//!
//! - pattern length above `EngineLimits::pattern_units_max` — rejected by the
//!   Rust pre-check without entering JS at all (`Resource`, "code-unit limit");
//! - nesting with groups — QuickJS's capture count cap (`Syntax`,
//!   "too many captures"; depth 1 000 already exceeds it);
//! - non-capturing nesting — libregexp's parser recursion against the C stack
//!   bound (`Resource`, "stack overflow"; depth 1 000 compiles, 10 000
//!   overflows). quickjs.c surfaces this as a SyntaxError, which the crate
//!   reclassifies because it is a resource limit, not a malformed pattern;
//! - a small `memory_bytes` — either the bootstrap cannot be evaluated
//!   (engine init fails closed) or building the pattern's code-unit bridge
//!   exhausts the JS heap (`Resource`, "out of memory").

#![cfg(feature = "regex-proto")]

use pilotdeck_regex_proto::{CompileError, EngineLimits, RegexEngine};

fn units(text: &str) -> Vec<u16> {
    text.encode_utf16().collect()
}

#[test]
fn pattern_length_cap_is_a_resource_error() {
    let engine = RegexEngine::new(EngineLimits::new()).expect("#1265 engine init");
    let cap = EngineLimits::new().pattern_units_max;
    let over = units(&"a".repeat(cap + 1));
    let error = engine
        .compile(&over, "")
        .expect_err("#1265 an over-cap pattern must be rejected");
    assert!(
        matches!(error, CompileError::Resource(ref message) if message.contains("code-unit limit")),
        "#1265 over-cap pattern must be a resource error: {error:?}"
    );
    let at = units(&"a".repeat(cap));
    engine
        .compile(&at, "")
        .expect("#1265 a pattern at the cap must compile");
}

#[test]
fn nesting_beyond_the_c_stack_is_a_resource_error() {
    let engine = RegexEngine::new(EngineLimits::new()).expect("#1265 engine init");
    // Depth 1 000 compiles; depth 10 000 exhausts the parser's recursion
    // against set_max_stack_size (512 KiB by default).
    let shallow = units(&format!("{}a{}", "(?:".repeat(1_000), ")".repeat(1_000)));
    engine
        .compile(&shallow, "")
        .expect("#1265 shallow nesting must compile");
    let deep = units(&format!("{}a{}", "(?:".repeat(10_000), ")".repeat(10_000)));
    let error = engine
        .compile(&deep, "")
        .expect_err("#1265 deep nesting must be rejected");
    assert!(
        matches!(error, CompileError::Resource(ref message) if message == "stack overflow"),
        "#1265 deep nesting must hit the stack limit as a resource error: {error:?}"
    );
    // Nesting with capture groups hits QuickJS's capture count cap first,
    // which is a genuine pattern rejection, not a resource condition.
    let grouped = units(&format!("{}a{}", "(".repeat(2_000), ")".repeat(2_000)));
    let error = engine
        .compile(&grouped, "")
        .expect_err("#1265 the capture cap must reject");
    assert!(
        matches!(error, CompileError::Syntax(ref message) if message == "too many captures"),
        "#1265 the capture cap must stay a syntax error: {error:?}"
    );
}

#[test]
fn a_memory_limit_that_cannot_boot_the_bootstrap_fails_engine_init() {
    // Measured: 64 KiB cannot evaluate the fixed bootstrap, so construction
    // itself fails closed instead of returning a half-alive engine.
    let error = RegexEngine::new(EngineLimits {
        memory_bytes: 64 * 1024,
        ..EngineLimits::new()
    })
    .expect_err("#1265 a 64 KiB heap must not boot");
    assert!(
        error.to_string().contains("Allocation failed"),
        "#1265 engine init under a tiny heap must report the allocation failure: {error}"
    );
}

#[test]
fn a_bridge_that_exhausts_the_heap_is_a_resource_error() {
    // Measured: 256 KiB boots the engine, but a 200 000-unit character class
    // exhausts the JS heap while the code-unit bridge is being built (or while
    // the RegExp compiles): a resource error either way, never a syntax one.
    let engine = RegexEngine::new(EngineLimits {
        memory_bytes: 256 * 1024,
        ..EngineLimits::new()
    })
    .expect("#1265 a 256 KiB heap boots the bootstrap");
    let big = units(&format!("[{}]", "abcdefghij".repeat(20_000)));
    let error = engine
        .compile(&big, "")
        .expect_err("#1265 the heap must reject the oversized class");
    assert!(
        matches!(error, CompileError::Resource(ref message) if message == "out of memory"),
        "#1265 heap exhaustion must be a resource error: {error:?}"
    );
    // The engine itself is still usable for ordinary patterns afterwards.
    engine
        .compile(&units("^a$"), "u")
        .expect("#1265 the engine keeps serving ordinary patterns");
}
