//! #3152 fix-r2 (review sibling#3): the proven-dead stderr sink, one
//! implementation for the two trees that need it. #3152 items 13+9
//! converged the lib tree's four failed-stderr probes into
//! `tests/common/failed_stderr_probe.rs`, but the integration tree kept
//! a line-for-line twin in `tests/console_panic_safety.rs`
//! (`Gateway::dead_stderr`, item 4's evidence base): two copies of a
//! rule whose parent-side probe write and `BrokenPipe` assertion are the
//! premise behind every spawn that uses the sink, so a future edit to
//! the probe bytes or the assertion could drift between the trees and
//! leave one side's "the sink is already dead" silently weaker. Both
//! trees now mount this one file (`#[path]`, the `source_inputs.rs`
//! precedent).
//!
//! Only unix: the socketpair sink is a unix-domain construct, and every
//! host this crate tests on (CI's ubuntu runner, the dev macmini) is
//! unix — `tests/common/write_stage.rs` sets the precedent for bare unix
//! gating in this tree.

use std::io::Write as _;

/// The failing stderr sink itself (#3152 item 9): a socketpair whose
/// peer is dropped up front, proven dead by a parent-side probe write
/// that must fail `BrokenPipe`, then handed to the child as its stderr
/// `Stdio` — every write the child makes fails with EPIPE before the
/// child even exists. A read-only fd is not a failing sink for this
/// purpose: `std::io::Stderr` redirects EBADF to a fresh `/dev/null`
/// and reports success, so only EPIPE reaches a panicking `eprintln!`.
pub(crate) fn dead_stderr(tag: &str) -> std::process::Stdio {
    let (write_half, read_half) = std::os::unix::net::UnixStream::pair()
        .unwrap_or_else(|error| panic!("{tag} create the dead stderr socketpair: {error:?}"));
    drop(read_half);
    let error = (&write_half)
        .write(b"dead stderr probe")
        .expect_err("the dead stderr sink accepted a write; the peer drop did not take effect");
    assert_eq!(
        error.kind(),
        std::io::ErrorKind::BrokenPipe,
        "{tag} the dead stderr sink's probe write failed with {error:?}"
    );
    let owned_fd: std::os::fd::OwnedFd = write_half.into();
    std::process::Stdio::from(owned_fd)
}
