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

pub fn stderr_skip_line(text: &str) {
    use std::io::Write;
    let _ = std::io::stderr().write_all(text.as_bytes());
    let _ = std::io::stderr().flush();
}
