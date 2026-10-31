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
    thread,
    time::{Duration, Instant},
};

use rustix::process::Signal;

use super::test_scratch::{OWNER_FILE, Scratch, write_executable, write_executable_bounded};

/// How long a test waits for the guard's kill report; the report is already
/// in the channel by the time `write_executable_bounded` returns, so this
/// only bounds a never-firing mutant.
const REPORT_TIMEOUT: Duration = Duration::from_secs(5);

/// How long the stranger below may take to record the EOF it read; it sleeps
/// past the write deadline first, so this only bounds a never-firing mutant
/// (#1141).
const MARKER_TIMEOUT: Duration = Duration::from_secs(8);

/// How far past the deadline a publication may take to return; a correct
/// one returns right after the writer reports its give-up (#1141).
const RETURN_GRACE: Duration = Duration::from_secs(2);

/// The name the stranger records its EOF observation under (#1141).
const STRANGER_EOF: &str = "stranger-eof";

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
/// publication returns (#1125 fix-r3 review P2-2). The stranger's `<&0`
/// is what keeps it on the read end: without an explicit redirection
/// `/bin/sh` points a background command's stdin at `/dev/null`, so the
/// read end would close with the helper (#1141, #1125 r4 review P2-1).
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

/// #1141 (#1125 r4 review P2-1): the helper is already dead — it `exit 0`s
/// on its own right after the spawn — while a stranger process still holds
/// the pipe's read end, so neither the helper's death nor its kill can end
/// the blocked write; only the writer's own deadline does. The observable
/// chain: the reap report shows an ordinary exit (a guard-killed helper
/// would report SIGKILL), the write fails with `TimedOut` (a read end
/// closed at the deadline would have surfaced as a broken pipe instead),
/// the publication returns within its deadline, and the stranger — which
/// sleeps past the deadline, reads until EOF, and records that in the
/// marker — proves the writer thread exited and with it dropped this
/// process's only copy of the write end: EOF reaches a reader only after
/// every write end is closed.
#[test]
fn a_dead_helper_with_a_stranger_reader_still_closes_the_write_end() {
    let scratch = Scratch::new("helper-dead-stranger");
    let marker = scratch.path().join(STRANGER_EOF);
    let (kill_sent, kill_reported) = mpsc::channel();
    let body = "\n".repeat(256 * 1024);
    let deadline = Duration::from_millis(1500);
    let started = Instant::now();
    let error = write_executable_bounded(
        &scratch.path().join("script"),
        &body,
        0o755,
        deadline,
        |_| dead_helper_with_a_stranger_reader(&marker),
        |status| {
            let _ = kill_sent.send(status);
        },
    )
    .expect_err("#1141 a dead helper with a stranger reader must fail its publication");
    assert!(
        started.elapsed() < deadline + RETURN_GRACE,
        "#1141 publication must return within its deadline"
    );
    assert!(
        error.kind() == io::ErrorKind::TimedOut,
        "#1141 deadline error must be TimedOut: {error}"
    );
    assert_reaped_after_a_natural_death(&kill_reported);
    let marker_deadline = Instant::now() + MARKER_TIMEOUT;
    while !marker.exists() && Instant::now() < marker_deadline {
        thread::sleep(Duration::from_millis(5));
    }
    assert!(
        marker.exists(),
        "#1141 the write end must be closed for the stranger to see EOF"
    );
    let expected: Vec<std::ffi::OsString> = vec![OWNER_FILE.into(), STRANGER_EOF.into()];
    assert_eq!(
        remaining_names(&scratch),
        expected,
        "#1141 dead helper residue"
    );
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
/// only the writer's deadline ends the write. The `<&0` keeps the stranger
/// on the read end — a plain `sleep 5 &` reads `/dev/null`, because
/// `/bin/sh` assigns background commands that stdin unless it is explicitly
/// redirected (#1141). The stranger is a `sleep 5`, so it lets go on its own
/// shortly after the 1.5-second deadline and never outlives the test by more
/// than a bounded moment.
fn stalled_behind_a_stranger_reader() -> process::Command {
    let mut helper = process::Command::new("/bin/sh");
    helper
        .arg("-c")
        .arg("sleep 5 <&0 & exec /bin/sleep 30")
        .arg("stalled-helper")
        .stdin(process::Stdio::piped());
    helper
}

/// `/bin/sh` `exit 0`s right away, so the helper is dead long before the
/// deadline, while a background stranger subshell keeps holding the pipe's
/// read end through the same `<&0` retention as above (#1141). The stranger
/// sleeps past the 1.5-second deadline, then reads until EOF — which
/// arrives only once every write end is closed — and records that it saw
/// it by creating `marker`.
fn dead_helper_with_a_stranger_reader(marker: &Path) -> process::Command {
    let mut helper = process::Command::new("/bin/sh");
    helper
        .arg("-c")
        .arg("( sleep 3; cat > /dev/null; : > \"$1\" ) <&0 & exit 0")
        .arg("dead-helper")
        .arg(marker)
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

/// The helper of the #1141 test dies on its own before the guard acts: the
/// guard's kill + wait on an already-exited child reports an ordinary
/// successful exit, while a helper the guard had to end would report
/// SIGKILL. A signal here means the helper was still alive at the deadline
/// and the interleaving under test never happened.
fn assert_reaped_after_a_natural_death(reported: &Receiver<io::Result<process::ExitStatus>>) {
    let status = reported
        .recv_timeout(REPORT_TIMEOUT)
        .expect("#1141 dead helper reap must be reported")
        .expect("#1141 dead helper must be reaped");
    use std::os::unix::process::ExitStatusExt;
    assert!(
        status.success() && status.signal().is_none(),
        "#1141 helper must have exited on its own, not ended by the guard: {status}"
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
