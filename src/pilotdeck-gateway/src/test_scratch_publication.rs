//! #1125 publication checks for `test_scratch::write_executable`.
//!
//! These live in a lib-only module rather than inside the shared
//! `tests/common/scratch.rs` so the integration-test crates that
//! `#[path]`-include that file do not each compile and run another copy
//! of them (#1125 fix-r2 review P2); only the copy compiled into the lib
//! target is bound by the `grep-etxtbsy` negative control. unix-only, like
//! the `/bin/sh` helpers and `rustix` calls they drive (#1125 fix-r3
//! review P2).
use std::{
    fs, io,
    path::Path,
    process,
    sync::mpsc::{self, Receiver},
    time::Duration,
};

use rustix::process::Signal;

use super::test_scratch::{OWNER_FILE, Scratch, write_executable, write_executable_bounded};

/// How long a test waits for the guard's kill report; the report is already
/// in the channel by the time `write_executable_bounded` returns, so this
/// only bounds a never-firing mutant.
const REPORT_TIMEOUT: Duration = Duration::from_secs(5);

/// #1125 fix-r1 review P2: a publication that fails after the helper
/// created the temporary name must remove it again, so a failed
/// `write_executable` leaves no `.tmp-` residue behind.
#[test]
fn failed_publication_removes_the_temporary_name() {
    let scratch = Scratch::new("write-executable");
    // Renaming a file onto an existing directory fails, so the helper's
    // write succeeds while the publication does not.
    let target = scratch.path().join("occupied");
    fs::create_dir(&target).expect("#1125 create occupied dir");
    let error = write_executable(&target, "#!/bin/sh\n", 0o755)
        .expect_err("#1125 publication must fail onto a directory");
    assert_ne!(
        error.kind(),
        io::ErrorKind::NotFound,
        "#1125 failure must come from the rename, not a missing path: {error}"
    );
    // Only the owner token and the occupied directory may remain.
    let expected: Vec<std::ffi::OsString> = vec![OWNER_FILE.into(), "occupied".into()];
    assert_eq!(
        remaining_names(&scratch),
        expected,
        "#1125 temp residue after failed publication"
    );
}

/// A helper that stalls after `cat` finished must be killed and reaped at
/// the deadline, with the temporary name removed (#1125 fix-r2 review P2).
#[test]
fn a_stalled_helper_is_killed_and_cleaned_up() {
    let scratch = Scratch::new("helper-stall");
    let (kill_sent, kill_reported) = mpsc::channel();
    let error = write_executable_bounded(
        &scratch.path().join("script"),
        "#!/bin/sh\n",
        0o755,
        Duration::from_millis(1500),
        stalled_after_cat,
        |status| {
            let _ = kill_sent.send(status);
        },
    )
    .expect_err("#1125 a stalled helper must fail its publication");
    assert!(
        error.kind() == io::ErrorKind::TimedOut,
        "#1125 deadline error must be TimedOut: {error}"
    );
    assert_killed_and_reaped(&kill_reported);
    assert_only_the_owner_token_remains(&scratch);
}

/// A helper that never reads its stdin must not wedge the publication in
/// an unbounded write: the body below is larger than any pipe buffer, so
/// the writer blocks and the deadline must fire instead (#1125 fix-r2
/// review P2).
#[test]
fn a_helper_that_never_reads_hits_the_write_deadline() {
    let scratch = Scratch::new("helper-stall-write");
    let (kill_sent, kill_reported) = mpsc::channel();
    let body = "\n".repeat(256 * 1024);
    let error = write_executable_bounded(
        &scratch.path().join("script"),
        &body,
        0o755,
        Duration::from_millis(1500),
        |_| stalled_without_reading(),
        |status| {
            let _ = kill_sent.send(status);
        },
    )
    .expect_err("#1125 a helper that never reads must fail its publication");
    assert!(
        error.kind() == io::ErrorKind::TimedOut,
        "#1125 deadline error must be TimedOut: {error}"
    );
    assert_killed_and_reaped(&kill_reported);
    assert_only_the_owner_token_remains(&scratch);
}

/// A stranger process — not the helper — can outlive the helper's kill
/// while holding the pipe's read end, so killing the helper cannot break
/// the writer out of its blocked write. Only the writer's own deadline
/// ends the write and drops the parent's copy of the write end before the
/// publication returns (#1125 fix-r3 review P2-2).
#[test]
fn a_stranger_on_the_read_end_still_hits_the_write_deadline() {
    let scratch = Scratch::new("helper-stall-stranger");
    let (kill_sent, kill_reported) = mpsc::channel();
    let body = "\n".repeat(256 * 1024);
    let error = write_executable_bounded(
        &scratch.path().join("script"),
        &body,
        0o755,
        Duration::from_millis(1500),
        |_| stalled_behind_a_stranger_reader(),
        |status| {
            let _ = kill_sent.send(status);
        },
    )
    .expect_err("#1125 a stranger on the read end must fail its publication");
    assert!(
        error.kind() == io::ErrorKind::TimedOut,
        "#1125 deadline error must be TimedOut: {error}"
    );
    assert_killed_and_reaped(&kill_reported);
    assert_only_the_owner_token_remains(&scratch);
}

/// `/bin/sh` copies its stdin into `temporary`, then `exec`s a sleep: the
/// write completes, the wait can never finish. The real `cat` helper cannot
/// be stalled deterministically this way because EOF always arrives.
fn stalled_after_cat(temporary: &Path) -> process::Command {
    let mut helper = process::Command::new("/bin/sh");
    helper
        .arg("-c")
        .arg("cat > \"$1\"; exec /bin/sleep 30")
        .arg("stalled-helper")
        .arg(temporary)
        .stdin(process::Stdio::piped());
    helper
}

/// `/bin/sh` `exec`s a sleep without ever reading its stdin, so a body
/// larger than the pipe buffer blocks the writer.
fn stalled_without_reading() -> process::Command {
    let mut helper = process::Command::new("/bin/sh");
    helper
        .arg("-c")
        .arg("exec /bin/sleep 30")
        .arg("stalled-helper")
        .stdin(process::Stdio::piped());
    helper
}

/// `/bin/sh` leaves a background `sleep` holding the pipe's read end, then
/// `exec`s its own sleep: killing the helper cannot close the read end, so
/// only the writer's deadline ends the write. The stranger is a `sleep 5`,
/// so it lets go on its own shortly after the 1.5-second deadline and never
/// outlives the test by more than a bounded moment.
fn stalled_behind_a_stranger_reader() -> process::Command {
    let mut helper = process::Command::new("/bin/sh");
    helper
        .arg("-c")
        .arg("sleep 5 & exec /bin/sleep 30")
        .arg("stalled-helper")
        .stdin(process::Stdio::piped());
    helper
}

/// The guard's kill report is the only liveness evidence used (#1125
/// fix-r3 review P0): it proves the helper was killed and reaped through
/// the original `Child`, so a pid that was recycled after the reaping can
/// neither be misread as alive nor signalled by mistake.
fn assert_killed_and_reaped(reported: &Receiver<io::Result<process::ExitStatus>>) {
    let status = reported
        .recv_timeout(REPORT_TIMEOUT)
        .expect("#1125 stalled helper kill must be reported")
        .expect("#1125 stalled helper must be killed and reaped");
    use std::os::unix::process::ExitStatusExt;
    assert_eq!(
        status.signal(),
        Some(Signal::KILL.as_raw()),
        "#1125 stalled helper must be killed and reaped, not left running"
    );
}

fn assert_only_the_owner_token_remains(scratch: &Scratch) {
    let expected: Vec<std::ffi::OsString> = vec![OWNER_FILE.into()];
    assert_eq!(
        remaining_names(scratch),
        expected,
        "#1125 stalled helper residue"
    );
}

fn remaining_names(scratch: &Scratch) -> Vec<std::ffi::OsString> {
    let mut names: Vec<_> = fs::read_dir(scratch.path())
        .expect("#1125 read scratch directory")
        .map(|entry| entry.expect("#1125 scratch entry").file_name())
        .collect();
    names.sort();
    names
}
