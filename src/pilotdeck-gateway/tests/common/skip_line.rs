//! #1263: skip lines must reach the default `cargo test` output.
//!
//! libtest captures the `eprintln!` macro for a passing test, so a `skip:`
//! reason written with the macro is invisible in the default run — the test
//! looks silently green (#1263's three-state leak). Writing through the stderr
//! handle directly bypasses the capture (the #448 unsafe_boundary precedent,
//! probed there on rustc 1.99.1: handle writes bypass the capture; the
//! macro's output does not). Shared by edgeclaw_heartbeat_indexer.rs,
//! write_tools.rs, grep_tool.rs, dump_request.rs and workspace_snapshot.rs
//! (`#[path]`), and by the crate's own unit tests through
//! `crate::test_skip_line` (lib.rs) — do not copy the writer.

/// #1637: machine-readable prefix for a whole-test self-skip line — a test
/// that skipped *itself* (not one scenario of many inside a test that still
/// ran) prints `<short-test-name>: skipped, …` through [`stderr_skip_line`]
/// with this prefix in front, so `tests/fixtures/negative_suite.py`'s
/// `passed()` can tell a self-skipped bound test from a genuinely passing
/// positive control. The per-scenario skip in tests/workspace_snapshot.rs's
/// replay deliberately does NOT use it: there the test body runs and only
/// one scenario is absent, which `passed()` must keep judging as a real
/// execution. Consumed as `^PDRS_TEST_SKIPPED <short-name>\b` (MULTILINE).
// The test crates that include this file via #[path] (write_tools.rs, …)
// do not use the marker, unlike the lib's own tests — same situation as
// scratch.rs's blanket allow, scoped here to the one constant.
#[allow(dead_code)]
pub const TEST_SKIPPED_MARKER: &str = "PDRS_TEST_SKIPPED ";

pub fn stderr_skip_line(text: &str) {
    use std::io::Write;
    let _ = std::io::stderr().write_all(text.as_bytes());
    let _ = std::io::stderr().flush();
}
