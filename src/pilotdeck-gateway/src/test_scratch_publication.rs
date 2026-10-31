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
    path::{Path, PathBuf},
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

/// How long the stranger below may take to record the EOF it read after the
/// go signal; EOF is already pending by then — the publication returned and
/// the writer dropped its write end — so this only bounds a never-firing
/// mutant (#1141).
const MARKER_TIMEOUT: Duration = Duration::from_secs(8);

/// How far past the deadline a publication may take to return; a correct
/// one returns right after the writer reports its give-up (#1141).
const RETURN_GRACE: Duration = Duration::from_secs(2);

/// How far past the go signal the stranger's EOF marker may land: the write
/// end was closed before the publication returned, so EOF is already pending
/// and only the stranger's own poll interval and the pipe drain separate the
/// two files' timestamps. A copy still held when the publication returned
/// holds EOF back past this grace (#1141 fix-r1 review P2-1).
const EOF_AFTER_GO_GRACE: Duration = Duration::from_secs(2);

/// How long the guard below may wait for the dead helper to publish the
/// stranger's group id; the helper writes it before it exits, so this only
/// bounds a helper that never spawned a stranger (#1141 fix-r1 review P1-1).
const STRANGER_GROUP_TIMEOUT: Duration = Duration::from_secs(2);

/// How long the guard below may take to confirm the stranger's process group
/// is empty after its kill; it only bounds a stranger that refuses to die
/// (#1141 fix-r1 review P1-1).
const STRANGER_REAP_TIMEOUT: Duration = Duration::from_secs(5);

/// The name the stranger records its EOF observation under (#1141).
const STRANGER_EOF: &str = "stranger-eof";

/// The go signal the test publishes once the publication has returned: the
/// stranger reads the pipe only after it, so the EOF observation is bound to
/// the return boundary instead of a fixed sleep past the deadline
/// (#1141 fix-r1 review P2-1).
const STRANGER_GO: &str = "stranger-go";

/// The name the dead helper publishes its process-group id under — it is the
/// group leader, so its pid is the only handle the test ever gets on the
/// stranger tree, whose members are its grandchildren (#1141 fix-r1 review
/// P1-1).
const STRANGER_GROUP: &str = "stranger-group";

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
/// and the publication returns within its deadline. The stranger — released
/// by a go signal the test publishes once the publication has returned —
/// then reads until EOF and records that in the marker, proving the writer
/// thread exited and with it dropped this process's only copy of the write
/// end: EOF reaches a reader only after every write end is closed. The
/// marker's timestamp against the go signal's binds that proof to the
/// return boundary — a copy still held when the publication returned holds
/// EOF back past the grace below (#1141 fix-r1 review P2-1). The stranger
/// parks in a bounded sleep after recording EOF rather than ending on its
/// own, so the [`StrangerGuard`] is the only thing that may collect it, on
/// every exit of this test (#1141 fix-r1 review P1-1): it kills the
/// stranger's process group, confirms the group is empty, and the test
/// asserts the stranger was collected before it returns.
#[test]
fn a_dead_helper_with_a_stranger_reader_still_closes_the_write_end() {
    let scratch = Scratch::new("helper-dead-stranger");
    let marker = scratch.path().join(STRANGER_EOF);
    // The go signal and the group-id file are test machinery rather than
    // observations of the publication, so they live in a scratch of their
    // own and the residue assertion keeps watching the publication's names.
    let signals = Scratch::new("helper-dead-stranger-signals");
    let go = signals.path().join(STRANGER_GO);
    let group_file = signals.path().join(STRANGER_GROUP);
    // Declared before the publication so every later exit of this test —
    // including a panic from inside the shared helper — unwinds through the
    // guard's Drop (#1141 fix-r1 review P1-1).
    let mut stranger = StrangerGuard::new(&group_file);
    let (kill_sent, kill_reported) = mpsc::channel();
    let body = "\n".repeat(256 * 1024);
    let deadline = Duration::from_millis(1500);
    let started = Instant::now();
    let error = write_executable_bounded(
        &scratch.path().join("script"),
        &body,
        0o755,
        deadline,
        |_| dead_helper_with_a_stranger_reader(&marker, &go, &group_file),
        |status| {
            let _ = kill_sent.send(status);
        },
    )
    .expect_err("#1141 a dead helper with a stranger reader must fail its publication");
    stranger.adopt(
        stranger_group(&group_file, STRANGER_GROUP_TIMEOUT)
            .expect("#1141 the dead helper must publish the stranger's group id"),
    );
    publish_go_signal(&go);
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
    // #1141 fix-r1 review P2-1: the check above proves the write end closes
    // eventually; this one proves it was closed by the time the publication
    // returned. The go signal was published after the return and the
    // stranger reads only after it, so EOF was already pending unless a
    // write end survived the return boundary to hold it back.
    let eof = fs::metadata(&marker)
        .expect("#1141 read the stranger's EOF marker")
        .modified()
        .expect("#1141 the stranger's EOF marker needs a modification time");
    let signaled = fs::metadata(&go)
        .expect("#1141 read the published go signal")
        .modified()
        .expect("#1141 the go signal needs a modification time");
    let since_go = eof.duration_since(signaled).unwrap_or_default();
    assert!(
        since_go < EOF_AFTER_GO_GRACE,
        "#1141 the write end must be closed by the time the publication returns, \
         not held {since_go:?} after the go signal"
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
/// read end through the same `<&0` retention as above (#1141). The helper
/// runs as its own process-group leader, so the stranger and everything it
/// runs stay in a group the test alone can end. The stranger waits for the
/// go signal the test publishes once the publication has returned (capped,
/// so even a test that never signals leaves it bounded), reads until EOF —
/// which arrives only once every write end is closed — records that by
/// creating `marker`, and then parks in a bounded sleep that only the test's
/// [`StrangerGuard`] may end, never EOF or a self-chosen lifetime
/// (#1141 fix-r1 review P1-1).
fn dead_helper_with_a_stranger_reader(
    marker: &Path,
    go: &Path,
    group_file: &Path,
) -> process::Command {
    let mut helper = process::Command::new("/bin/sh");
    helper
        .arg("-c")
        .arg(
            "( i=0
               while [ ! -e \"$2\" ] && [ \"$i\" -lt 600 ]; do i=$((i+1)); /bin/sleep 0.1; done
               cat > /dev/null
               : > \"$1\"
               exec /bin/sleep 30 ) <&0 &
               echo $$ > \"$3\"
               exit 0",
        )
        .arg("dead-helper")
        .arg(marker)
        .arg(go)
        .arg(group_file)
        .stdin(process::Stdio::piped())
        .process_group(0);
    helper
}

/// Ends the stranger the dead-helper test leaves behind, on every exit of
/// that test (#1141 fix-r1 review P1-1). The group id arrives only once the
/// dead helper has published it, so a panic from before that point is
/// covered by Drop re-reading the file: the helper spawns the stranger
/// before it exits, so the file is already complete whenever the panic
/// happens past the spawn.
struct StrangerGuard {
    group_file: PathBuf,
    group: Option<Pid>,
    ended: bool,
}

impl StrangerGuard {
    fn new(group_file: &Path) -> Self {
        Self {
            group_file: group_file.to_path_buf(),
            group: None,
            ended: false,
        }
    }

    /// Take over the stranger once its group id is known; until then Drop
    /// below re-reads the file, so a panic before this point is covered too.
    fn adopt(&mut self, group: Pid) {
        self.group = Some(group);
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

impl Drop for StrangerGuard {
    fn drop(&mut self) {
        if !self.ended && self.group.is_none() {
            // A panic unwound before the group id was adopted: the helper
            // may still have spawned the stranger, so give its file a
            // bounded chance to appear before deciding there is nothing to
            // end.
            self.group = stranger_group(&self.group_file, STRANGER_GROUP_TIMEOUT);
        }
        // The panic and timeout paths must not assert — a panic raised while
        // unwinding would abort the process — but the kill stays bounded.
        let _ = self.end();
    }
}

/// Read the stranger's process-group id, giving the dead helper a bounded
/// moment to publish it; `None` when the file never appears, which on the
/// Drop paths means the helper never spawned a stranger at all.
fn stranger_group(group_file: &Path, wait: Duration) -> Option<Pid> {
    let deadline = Instant::now() + wait;
    loop {
        let published = fs::read_to_string(group_file)
            .ok()
            .and_then(|text| text.trim().parse::<i32>().ok())
            .filter(|pid| *pid > 0);
        if published.is_some() || Instant::now() >= deadline {
            return published.and_then(Pid::from_raw);
        }
        thread::sleep(Duration::from_millis(5));
    }
}

/// Publish the go signal atomically — write a staging file, then rename it
/// into place (#166 r8) — so the stranger waits for the rename, never for a
/// partially written file. Its timestamp is the return boundary the EOF
/// observation is judged against (#1141 fix-r1 review P2-1).
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
