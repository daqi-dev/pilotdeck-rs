//! #3152 item 8: the test-support receipt family's console writes.
//! Test children — re-exec'd test binaries their parents drive (the
//! `WRITE_ERROR=` spill markers, the host integration peers, the
//! rerun watcher) — print one receipt line each, and a panicking
//! console macro (`println!` / `eprintln!`) kills the very child the
//! receipt reports on: the parent's read then times out and the
//! probe fails without its evidence. Same shape as #2558's production
//! `console_log_line`, local to the test tree — the production writer
//! stays a production concern, and test code does not reach into
//! `native_process` for a receipt.
//!
//! One buffer, one `write_all`: a line and its newline leave in a
//! single write, so two threads can never interleave between them
//! (#3152 item 11's rule, same reasoning).

use std::io::Write;

/// One best-effort stdout receipt line. The write error is dropped —
/// a failed write means the parent is gone; its own probe fails on
/// the missing marker instead of this child's panic.
pub(crate) fn receipt_line(message: &str) {
    let mut line = String::with_capacity(message.len() + 1);
    line.push_str(message);
    line.push('\n');
    let _ = std::io::stdout().write_all(line.as_bytes());
}

/// One best-effort stderr receipt line (`test_process.rs`'s rerun
/// watcher): the stdout channel there carries receipts other
/// consumers parse, so its progress note stays on stderr.
pub(crate) fn receipt_line_stderr(message: &str) {
    let mut line = String::with_capacity(message.len() + 1);
    line.push_str(message);
    line.push('\n');
    let _ = std::io::stderr().write_all(line.as_bytes());
}
