//! #1263: skip lines must reach the default `cargo test` output.
//!
//! libtest captures the `eprintln!` macro for a passing test, so a `skip:`
//! reason written with the macro is invisible in the default run — the test
//! looks silently green (#1263's three-state leak). Writing through the stderr
//! handle directly bypasses the capture (the #448 unsafe_boundary precedent,
//! probed there on rustc 1.99.1: handle writes bypass the capture; the
//! macro's output does not). Loaded directly (`#[path]`) by the files that
//! call the writer themselves — unsafe_boundary.rs, grep_tool.rs,
//! write_tools.rs, workspace_snapshot.rs, and the lib root as
//! `crate::test_skip_line` — and, nested, by permission_faults.rs for
//! `skip_or_require`; every other crate reaches it only through that pair
//! helper (fix-r1 removed the unused direct includes). Do not copy the writer.

pub fn stderr_skip_line(text: &str) {
    use std::io::Write;
    let _ = std::io::stderr().write_all(text.as_bytes());
    let _ = std::io::stderr().flush();
}
