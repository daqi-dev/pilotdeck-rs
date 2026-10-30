//! #1125 publication checks for `test_scratch::write_executable`.
//!
//! These live in a lib-only module rather than inside the shared
//! `tests/common/scratch.rs` so the integration-test crates that
//! `#[path]`-include that file do not each compile and run another copy
//! of them (#1125 fix-r2 review P2); only the copy compiled into the lib
//! target is bound by the `grep-etxtbsy` negative control.
use std::{fs, io, path::Path, process, time::Duration};

use rustix::process::{Pid, Signal, kill_process};

use super::test_scratch::{OWNER_FILE, Scratch, write_executable, write_executable_bounded};

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
    let mut names: Vec<_> = fs::read_dir(scratch.path())
        .expect("#1125 read scratch directory")
        .map(|entry| entry.expect("#1125 scratch entry").file_name())
        .collect();
    names.sort();
    // Only the owner token and the occupied directory may remain.
    let expected: Vec<std::ffi::OsString> = vec![OWNER_FILE.into(), "occupied".into()];
    assert_eq!(
        names, expected,
        "#1125 temp residue after failed publication"
    );
}

/// A helper that stalls after `cat` finished must be killed and reaped at
/// the deadline, with the temporary name removed (#1125 fix-r2 review P2).
/// The helper records its own pid and `exec`s a sleep, so the kill is
/// observable; the real `cat` helper cannot be stalled deterministically
/// this way because EOF always arrives.
#[test]
fn a_stalled_helper_is_killed_and_cleaned_up() {
    let scratch = Scratch::new("helper-stall");
    let pid_file = scratch.path().join("helper.pid");
    let error = write_executable_bounded(
        &scratch.path().join("script"),
        "#!/bin/sh\n",
        0o755,
        Duration::from_millis(1500),
        |temporary| stalled_after_cat(&pid_file, temporary),
    )
    .expect_err("#1125 a stalled helper must fail its publication");
    assert!(
        error.kind() == io::ErrorKind::TimedOut,
        "#1125 deadline error must be TimedOut: {error}"
    );
    assert!(
        !stalled_helper_is_alive(&pid_file),
        "#1125 stalled helper must be killed and reaped"
    );
    let expected: Vec<std::ffi::OsString> = vec![OWNER_FILE.into(), "helper.pid".into()];
    assert_eq!(
        remaining_names(&scratch),
        expected,
        "#1125 stalled helper residue"
    );
}

/// A helper that never reads its stdin must not wedge the publication in
/// an unbounded write: the body below is larger than any pipe buffer, so
/// the writer thread blocks and the deadline must fire instead (#1125
/// fix-r2 review P2).
#[test]
fn a_helper_that_never_reads_hits_the_write_deadline() {
    let scratch = Scratch::new("helper-stall-write");
    let pid_file = scratch.path().join("helper.pid");
    let body = "\n".repeat(256 * 1024);
    let error = write_executable_bounded(
        &scratch.path().join("script"),
        &body,
        0o755,
        Duration::from_millis(1500),
        |_| stalled_without_reading(&pid_file),
    )
    .expect_err("#1125 a helper that never reads must fail its publication");
    assert!(
        error.kind() == io::ErrorKind::TimedOut,
        "#1125 deadline error must be TimedOut: {error}"
    );
    assert!(
        !stalled_helper_is_alive(&pid_file),
        "#1125 stalled helper must be killed and reaped"
    );
    let expected: Vec<std::ffi::OsString> = vec![OWNER_FILE.into(), "helper.pid".into()];
    assert_eq!(
        remaining_names(&scratch),
        expected,
        "#1125 stalled helper residue"
    );
}

/// `/bin/sh` records its own pid, copies its stdin into `temporary`, then
/// `exec`s a sleep: the write completes, the wait can never finish.
fn stalled_after_cat(pid_file: &Path, temporary: &Path) -> process::Command {
    let mut helper = process::Command::new("/bin/sh");
    helper
        .arg("-c")
        .arg("printf %s $$ > \"$1\"; cat > \"$2\"; exec /bin/sleep 30")
        .arg("stalled-helper")
        .arg(pid_file)
        .arg(temporary)
        .stdin(process::Stdio::piped());
    helper
}

/// `/bin/sh` records its own pid and `exec`s a sleep without ever reading
/// its stdin, so a body larger than the pipe buffer blocks the writer.
fn stalled_without_reading(pid_file: &Path) -> process::Command {
    let mut helper = process::Command::new("/bin/sh");
    helper
        .arg("-c")
        .arg("printf %s $$ > \"$1\"; exec /bin/sleep 30")
        .arg("stalled-helper")
        .arg(pid_file)
        .stdin(process::Stdio::piped());
    helper
}

/// `SIGKILL` doubles as the liveness probe: a helper that was killed and
/// reaped reports `ESRCH`, while a live one is terminated here so a
/// failing control cannot leave the 30-second sleep behind.
fn stalled_helper_is_alive(pid_file: &Path) -> bool {
    let pid: i32 = fs::read_to_string(pid_file)
        .expect("#1125 read stalled helper pid")
        .trim()
        .parse()
        .expect("#1125 parse stalled helper pid");
    kill_process(
        Pid::from_raw(pid).expect("#1125 valid stalled helper pid"),
        Signal::KILL,
    )
    .is_ok()
}

fn remaining_names(scratch: &Scratch) -> Vec<std::ffi::OsString> {
    let mut names: Vec<_> = fs::read_dir(scratch.path())
        .expect("#1125 read scratch directory")
        .map(|entry| entry.expect("#1125 scratch entry").file_name())
        .collect();
    names.sort();
    names
}
