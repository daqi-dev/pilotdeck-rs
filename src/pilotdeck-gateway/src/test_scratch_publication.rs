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
    panic::{AssertUnwindSafe, catch_unwind},
    path::Path,
    process,
    sync::mpsc::{self, Receiver},
    thread,
    time::{Duration, Instant},
};

use rustix::process::{Pid, Signal};

use super::test_scratch::{OWNER_FILE, Scratch, write_executable, write_executable_bounded};
use std::os::unix::process::CommandExt;

/// How long a test waits for the guard's kill report; the report is already
/// in the channel by the time `write_executable_bounded` returns, so this
/// only bounds a never-firing mutant.
const REPORT_TIMEOUT: Duration = Duration::from_secs(5);

/// How long the stranger below may take to record its write-end observation
/// after the go signal; the observation is already decided by then — the
/// publication returned and with it either every write end is closed or one
/// is held for good — so this only bounds a never-firing stranger.
const MARKER_TIMEOUT: Duration = Duration::from_secs(8);

/// How far past the deadline a publication may take to return; a correct
/// one returns right after the writer reports its give-up (#1141).
const RETURN_GRACE: Duration = Duration::from_secs(2);

/// How long the panic test below may wait for the stranger to signal that it
/// started; the stranger publishes that signal as its first act, so this only
/// bounds a stranger that never started (#1141 fix-r2 review P1-1).
const STRANGER_STARTED_TIMEOUT: Duration = Duration::from_secs(10);

/// How long the guard below may take to confirm the stranger's process group
/// is empty after its kill; it only bounds a stranger that refuses to die
/// (#1141 fix-r1 review P1-1).
const STRANGER_REAP_TIMEOUT: Duration = Duration::from_secs(5);

/// The name the stranger records its write-end observation under (#1141).
const STRANGER_EOF: &str = "stranger-eof";

/// The go signal the test publishes once the publication has returned: the
/// stranger reads the pipe only after it, so the write-end observation is
/// bound to the return boundary instead of a fixed sleep past the deadline
/// (#1141 fix-r1 review P2-1).
const STRANGER_GO: &str = "stranger-go";

/// The signal the stranger publishes as its first act; the panic test below
/// waits for it so its deliberate panic lands once the stranger provably
/// exists but no publication step has run yet (#1141 fix-r2 review P1-1).
const STRANGER_STARTED: &str = "stranger-started";

/// The panic the `spawned` hook of the panic test below raises once the
/// stranger has started; comparing the caught payload against it keeps a
/// hook that failed earlier — a stranger that never signalled — from passing
/// as the deliberate window panic (#1141 fix-r2 review P1-1).
const DELIBERATE_PANIC: &str = "#1141 deliberate panic while the stranger is running";

/// The stranger the dead-helper tests leave holding the pipe's read end
/// (#1141, #1125 r4 review P2-1): after the go signal it drains the pipe
/// with a zero-timeout `select` on its stdin and classifies what it sees —
/// a 0-byte read means EOF was pending, i.e. every write end was closed
/// before the observation, while a not-ready read end means one is still
/// held (the writer is gone by then, so no data can ever arrive and make it
/// ready; the state is monotone, which is what replaces a timing grace).
/// It publishes the verdict — and the `started` signal it publishes as its
/// first act — atomically through a staging file + rename (#166 r8), and
/// then parks in a bounded sleep that only the test's [`StrangerGuard`] may
/// end, never EOF or a self-chosen lifetime. `/bin/sh` itself cannot make a
/// non-blocking read, so the stranger is this `python3` probe (`python3` is
/// an established dependency of the gateway host runtime and of tests:
/// `host/connection.rs`, `pilotdeck-yaml/tests/resource_scaling.rs`); the
/// script contains no single quotes, so it embeds verbatim into the
/// helper's single-quoted `-c` argument below.
const STRANGER_PROBE: &str = r#"
import os, select, sys, time
marker, go, started = sys.argv[1], sys.argv[2], sys.argv[3]
def publish(path, text):
    staging = path + ".staging"
    with open(staging, "w") as handle:
        handle.write(text)
    os.rename(staging, path)
publish(started, "started")
deadline = time.monotonic() + 60.0
while not os.path.exists(go) and time.monotonic() < deadline:
    time.sleep(0.05)
while True:
    if not select.select([0], [], [], 0)[0]:
        publish(marker, "write-end-still-held")
        break
    if not os.read(0, 65536):
        publish(marker, "eof")
        break
time.sleep(30)
"#;

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
        // These helpers spawn no stranger, so nobody consumes the spawn id.
        |_| {},
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
        // This helper spawns no stranger, so nobody consumes the spawn id.
        |_| {},
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
        // This helper's stranger ends on its own within a bounded moment
        // (#1141), so nobody consumes the spawn id either.
        |_| {},
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
/// and the publication returns within its deadline. The stranger — released
/// by a go signal the test publishes once the publication has returned —
/// then observes the pipe with the non-blocking probe and records the
/// verdict in the marker, proving the writer thread exited and with it
/// dropped this process's only copy of the write end: EOF reaches a reader
/// only after every write end is closed, and the go signal was published
/// after the return, so a copy still held past that boundary is observed
/// as `write-end-still-held` instead of `eof` (#1141 fix-r2 review P2-1,
/// which replaced the r1 marker-timestamp judgment and its fixed grace).
/// The stranger parks in a bounded sleep after recording the verdict rather
/// than ending on its own, so the [`StrangerGuard`] is the only thing that
/// may collect it, on every exit of this test (#1141 fix-r1 review P1-1):
/// it kills the stranger's process group, confirms the group is empty, and
/// the test asserts the stranger was collected before it returns.
#[test]
fn a_dead_helper_with_a_stranger_reader_still_closes_the_write_end() {
    let scratch = Scratch::new("helper-dead-stranger");
    let marker = scratch.path().join(STRANGER_EOF);
    // The go and started signals are test machinery rather than observations
    // of the publication, so they live in a scratch of their own and the
    // residue assertion keeps watching the publication's names.
    let signals = Scratch::new("helper-dead-stranger-signals");
    let go = signals.path().join(STRANGER_GO);
    let started = signals.path().join(STRANGER_STARTED);
    // Declared before the publication so every later exit of this test —
    // including a panic from inside the shared helper — unwinds through the
    // guard's Drop (#1141 fix-r1 review P1-1).
    let mut stranger = StrangerGuard::new();
    let (kill_sent, kill_reported) = mpsc::channel();
    let body = "\n".repeat(256 * 1024);
    let deadline = Duration::from_millis(1500);
    let begin = Instant::now();
    let error = write_executable_bounded(
        &scratch.path().join("script"),
        &body,
        0o755,
        deadline,
        |_| dead_helper_with_a_stranger_reader(&marker, &go, &started),
        // #1141 fix-r2 review P1-1: the helper runs as its own group leader
        // (`process_group(0)` below), so its spawn-returned id is the group
        // the stranger lands in — adopted here, before the helper or the
        // stranger can do anything at all.
        |pid| stranger.adopt(helper_group(pid)),
        |status| {
            let _ = kill_sent.send(status);
        },
    )
    .expect_err("#1141 a dead helper with a stranger reader must fail its publication");
    assert!(
        begin.elapsed() < deadline + RETURN_GRACE,
        "#1141 publication must return within its deadline"
    );
    assert!(
        error.kind() == io::ErrorKind::TimedOut,
        "#1141 deadline error must be TimedOut: {error}"
    );
    assert_reaped_after_a_natural_death(&kill_reported);
    assert!(
        stranger.owns_group().is_some(),
        "#1141 the guard must own the stranger's group from the spawn on"
    );
    publish_go_signal(&go);
    let marker_deadline = Instant::now() + MARKER_TIMEOUT;
    while !marker.exists() && Instant::now() < marker_deadline {
        thread::sleep(Duration::from_millis(5));
    }
    let observation = fs::read_to_string(&marker)
        .expect("#1141 the stranger must record its write-end observation");
    // #1141 fix-r2 review P2-1: the probe distinguishes the two pipe states
    // directly — every write end already closed (a 0-byte read) versus one
    // still held past the return boundary (a not-ready read end) — so the
    // verdict, not a timestamp difference, is the judgment.
    assert!(
        observation == "eof",
        "#1141 the write end must be closed by the time the publication returns, \
         the stranger observed {observation:?}"
    );
    stranger
        .end()
        .expect("#1141 the stranger must be killed and reaped by the test's guard");
    let expected: Vec<std::ffi::OsString> = vec![OWNER_FILE.into(), STRANGER_EOF.into()];
    assert_eq!(
        remaining_names(&scratch),
        expected,
        "#1141 dead helper residue"
    );
}

/// #1141 fix-r2 review P1-1 (Codex): the stranger must be owned from the
/// moment the helper is spawned — not from a group id the helper child
/// publishes later, which left a window where a panic could not find the
/// group. This test panics deliberately inside that window — once the
/// stranger has signalled it started, but before the publication takes any
/// further step (its stdin is not even taken yet) — and catches the unwind:
/// the inner `ReapGuard` ends the helper while the outer [`StrangerGuard`]
/// still holds the spawn-returned group id, so it can kill the stranger's
/// group and confirm it is empty. The assertions run explicitly because
/// `catch_unwind` stops the unwind before the guard's own Drop would.
#[test]
fn a_panic_while_the_stranger_runs_still_ends_it() {
    let scratch = Scratch::new("helper-panic-stranger");
    // The panic never reaches the publication's assertions, so the marker,
    // go, and started signals all live in a scratch of their own and the
    // residue assertion watches the publication's names alone.
    let signals = Scratch::new("helper-panic-stranger-signals");
    let marker = signals.path().join(STRANGER_EOF);
    let go = signals.path().join(STRANGER_GO);
    let started = signals.path().join(STRANGER_STARTED);
    let mut stranger = StrangerGuard::new();
    let (kill_sent, _kill_reported) = mpsc::channel();
    let body = "\n".repeat(256 * 1024);
    let publication = catch_unwind(AssertUnwindSafe(|| {
        write_executable_bounded(
            &scratch.path().join("script"),
            &body,
            0o755,
            Duration::from_millis(1500),
            |_| dead_helper_with_a_stranger_reader(&marker, &go, &started),
            |pid| {
                stranger.adopt(helper_group(pid));
                wait_for_signal(&started, STRANGER_STARTED_TIMEOUT)
                    .expect("#1141 the stranger must signal that it started");
                panic!("{DELIBERATE_PANIC}");
            },
            |status| {
                let _ = kill_sent.send(status);
            },
        )
    }))
    .expect_err("#1141 the publication must panic while the stranger is running");
    let message = publication
        .downcast_ref::<&str>()
        .copied()
        .or_else(|| publication.downcast_ref::<String>().map(String::as_str))
        .unwrap_or("");
    assert_eq!(
        message, DELIBERATE_PANIC,
        "#1141 the caught panic must be the deliberate in-window one: {message:?}"
    );
    assert!(
        stranger.owns_group().is_some(),
        "#1141 the guard must own the stranger's group from the spawn on"
    );
    stranger
        .end()
        .expect("#1141 the stranger must still be killed and confirmed gone after a panic");
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
/// deadline, while a background stranger keeps holding the pipe's read end
/// through the same `<&0` retention as above (#1141). The helper runs as its
/// own process-group leader (`process_group(0)`), so the stranger — and
/// everything it runs — stays in a group the test alone can end, and whose
/// id is the spawn-returned one the `spawned` hook hands over: the group
/// exists from the spawn on, so it never depends on the helper (or the
/// stranger) publishing anything (#1141 fix-r2 review P1-1). The stranger
/// itself is the [`STRANGER_PROBE`] python process: it waits for the go
/// signal the test publishes once the publication has returned (capped, so
/// even a test that never signals leaves it bounded), observes the pipe,
/// records the verdict, and then parks in a bounded sleep that only the
/// test's [`StrangerGuard`] may end, never EOF or a self-chosen lifetime
/// (#1141 fix-r1 review P1-1).
fn dead_helper_with_a_stranger_reader(
    marker: &Path,
    go: &Path,
    started: &Path,
) -> process::Command {
    let mut helper = process::Command::new("/bin/sh");
    helper
        .arg("-c")
        .arg(format!(
            "python3 -c '{STRANGER_PROBE}' \"$1\" \"$2\" \"$3\" <&0 &
               exit 0"
        ))
        .arg("dead-helper")
        .arg(marker)
        .arg(go)
        .arg(started)
        .stdin(process::Stdio::piped())
        .process_group(0);
    helper
}

/// The helper's spawn-returned id, which `process_group(0)` above also made
/// the process-group id the stranger lands in (#1141 fix-r2 review P1-1).
fn helper_group(id: u32) -> Pid {
    Pid::from_raw(id as i32).expect("#1141 the helper's id must be a valid group id")
}

/// Ends the stranger the dead-helper tests leave behind, on every exit of
/// those tests (#1141 fix-r1 review P1-1, #1141 fix-r2 review P1-1). The
/// group id is handed over by the `spawned` hook the moment the helper
/// exists, so the guard is armed before the stranger can even be forked: a
/// helper that dies before forking it, a group file that is never written,
/// an in-flight publication that panics — the guard already holds the group,
/// and a `None` group means the spawn never succeeded, so nothing was ever
/// forked and there is nothing to end.
struct StrangerGuard {
    group: Option<Pid>,
    ended: bool,
}

impl StrangerGuard {
    fn new() -> Self {
        Self {
            group: None,
            ended: false,
        }
    }

    /// Take over the stranger's group at the spawn, through the `spawned`
    /// hook (#1141 fix-r2 review P1-1).
    fn adopt(&mut self, group: Pid) {
        self.group = Some(group);
    }

    /// The owned group, if the spawn handed one over; the tests assert this
    /// so a regression that never adopts cannot pass vacuously (#1141).
    fn owns_group(&self) -> Option<Pid> {
        self.group
    }

    /// Kill the stranger's whole process group and confirm the group is
    /// empty. The group holds only the dead helper's descendants, and it
    /// cannot have been recycled while the stranger is still parked in it.
    fn end(&mut self) -> io::Result<()> {
        if self.ended {
            return Ok(());
        }
        let Some(group) = self.group else {
            return Ok(());
        };
        super::process_group::kill(Some(group), Signal::KILL);
        let deadline = Instant::now() + STRANGER_REAP_TIMEOUT;
        while super::process_group::group_exists(group) && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(5));
        }
        if super::process_group::group_exists(group) {
            return Err(io::Error::other(
                "#1141 the stranger's process group survived the guard's kill",
            ));
        }
        self.ended = true;
        Ok(())
    }
}

impl Default for StrangerGuard {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for StrangerGuard {
    fn drop(&mut self) {
        // The panic and timeout paths must not assert — a panic raised while
        // unwinding would abort the process — but the kill stays bounded.
        let _ = self.end();
    }
}

/// Wait until the stranger has published `signal`, bounded by `wait`; the
/// stranger publishes the signal as its first act, so the bound only covers
/// a stranger that never started (#1141 fix-r2 review P1-1).
fn wait_for_signal(signal: &Path, wait: Duration) -> io::Result<()> {
    let deadline = Instant::now() + wait;
    while !signal.exists() && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(5));
    }
    if signal.exists() {
        Ok(())
    } else {
        Err(io::Error::other(
            "#1141 the stranger never signalled that it started",
        ))
    }
}

/// Publish the go signal atomically — write a staging file, then rename it
/// into place (#166 r8) — so the stranger waits for the rename, never for a
/// partially written file. The stranger reads the pipe only after it, which
/// binds the write-end observation to the return boundary instead of a
/// fixed sleep past the deadline (#1141 fix-r1 review P2-1, fix-r2 review
/// P2-1).
fn publish_go_signal(go: &Path) {
    let staging = go.with_extension("staging");
    fs::write(&staging, b"go").expect("#1141 write the go signal's staging file");
    fs::rename(&staging, go).expect("#1141 publish the go signal");
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
