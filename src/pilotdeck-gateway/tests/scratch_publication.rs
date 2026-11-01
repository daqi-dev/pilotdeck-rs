//! #1125/#1141/#1155 publication checks for `scratch::write_executable`.
//!
//! These live in their own integration-test binary — a dedicated process —
//! rather than inside the shared `tests/common/scratch.rs`, so the ~35
//! integration crates that `#[path]`-include that file do not each compile
//! and run another copy of them (#1125 fix-r2 review P2), and rather than
//! as a lib test module, because the child-subreaper lease the stranger
//! guards below take is a process-wide flag (#1155 fix-r1 review P1):
//! while any guard holds it, every orphan a sibling test leaves behind is
//! reparented to this process instead of init, and libtest's thread pool
//! never waits for those — the zombie stays in whatever process group a
//! parallel test is watching and keeps `group_exists` true forever. In the
//! shared lib test process that deterministically broke the parallel grep
//! group tests (fix-r1 probe: 5 of 7 grep group tests watchdog-expired in
//! each of 3 dense runs, `_tmp/20261026-issue-1155/fix-r1/probe/
//! dense-*.log`). Here the lease can adopt only orphans of these
//! publication tests, and every guard below reaps exactly its own: by
//! group (`waitpgid`) or — Linux only — by the stranger's published pid.
//! unix-only, like the `/bin/sh` helpers and `rustix` calls they drive
//! (#1125 fix-r3 review P2).
//!
//! #1155 tightens four of those contracts: the write end is proven closed
//! at the return by a `/proc` scan that runs inside the publication — no
//! observation gap is left after the return for a released copy to hide in
//! (r3 review P2-1); the stranger is reaped by the test process itself —
//! a child subreaper for the guard's lifetime — never left for PID 1 to
//! collect (r3 review P2-2); the helper's and the test's started-wait caps
//! are one derived constant (r4 review P2), and the write deadline starts
//! only once the `spawned` hook's handshake is done, so that wait cannot
//! consume the write budget it is meant to bound (r4 review P2).
#![cfg(unix)]

#[path = "common/scratch.rs"]
mod scratch;

use std::{
    cell::Cell,
    fs, io,
    panic::{AssertUnwindSafe, catch_unwind},
    path::Path,
    process,
    sync::mpsc::{self, Receiver},
    thread,
    time::{Duration, Instant},
};

use rustix::process::{Pid, Signal};
use std::os::unix::process::CommandExt;

#[cfg(target_os = "linux")]
use std::sync::Mutex;

use scratch::{OWNER_FILE, Scratch, write_executable, write_executable_bounded};

/// How long a test waits for the guard's kill report; the report is already
/// in the channel by the time `write_executable_bounded` returns, so this
/// only bounds a never-firing mutant.
const REPORT_TIMEOUT: Duration = Duration::from_secs(5);

/// How far past the deadline a publication may take to return; a correct
/// one returns right after the writer reports its give-up (#1141).
const RETURN_GRACE: Duration = Duration::from_secs(2);

/// How long the tests below may wait for the stranger to signal that it
/// started; the stranger publishes that signal only once it has confirmed it
/// holds the pipe's read end, so this only bounds a stranger that never
/// started or never got the read end (#1141 fix-r2 review P1-1, fix-r3).
/// The helper's own bounded started-wait is derived from this same constant
/// (#1155 r4 review P2), so there is one cap — not two to drift apart; the
/// r4 review found the helper waiting 5 s against the test's 10 s.
const STRANGER_STARTED_TIMEOUT: Duration = Duration::from_secs(10);

/// How long the guard below may take to confirm the stranger's process group
/// is empty after its kill; it only bounds a stranger that refuses to die
/// (#1141 fix-r1 review P1-1).
const STRANGER_REAP_TIMEOUT: Duration = Duration::from_secs(5);

/// #1155 r3 review P2-2: how long the guard below may take to reap the
/// stranger itself after its kill; the stranger is this process's child
/// under the subreaper lease, so this only bounds a reaping that never
/// completes.
#[cfg(target_os = "linux")]
const STRANGER_PID_REAP_TIMEOUT: Duration = Duration::from_secs(5);

/// #1155 r4 review P2 (fix-r1, both reviews): how long the stranger below
/// waits between confirming its stdin is the pipe and publishing `started`
/// — 8 s, four fifths of the unified 10 s cap above, and comfortably past
/// the 5 s a reverted helper's wait would hardcode on its own wall clock.
/// A round-counted mutant could stretch past a fixed delay under load
/// (each round is a `/bin/sleep 0.05` plus its fork/exec), so the helper's
/// bounded wait below is wall-clock (`date +%s`), which load cannot
/// stretch: a 5 s cap gives up 3 s of real time before the delayed
/// `started` appears, on any host.
const STRANGER_STARTED_DELAY: Duration = Duration::from_secs(8);

/// How long the helper below may take to record its write-end observation
/// after the go signal: the go decision is made on an arbitrary scheduler and
/// every observation is bounded to complete long before this runs out (#1141
/// fix-r3 review P1-2).
const MARKER_TIMEOUT: Duration = Duration::from_secs(8);

/// #1155 r4 review P2: how far below the deadline the measured write phase
/// may land; a healthy writer cannot give up before its own expiry — the
/// deadline now starts where the `spawned` hook returns — so the floor only
/// absorbs the instant between the hook's return and the expiry computation.
const DEADLINE_FLOOR_SLACK: Duration = Duration::from_millis(100);

/// #1155 r3 review P2-1: how long a write-end candidate found by the
/// return-boundary scan gets to disappear before it is declared a survivor.
/// A fork of this process that has not exec'd yet holds an inherited copy of
/// the write end for its fork-to-exec window — O_CLOEXEC only closes it at
/// that exec, the same pre-exec window #1125 was about — and such a copy is
/// on its way out, not a leak; the helpers of the sibling tests below are
/// spawned through that window while a scan may be running, so the scan
/// would otherwise flag a transient on the healthy path. No new copy can
/// appear once the writer thread is finished, so a candidate that persists
/// through the grace is one that really survives the return.
#[cfg(target_os = "linux")]
const TRANSIENT_WRITE_END_GRACE: Duration = Duration::from_millis(500);

/// The name the stranger records its write-end observation under (#1141).
const STRANGER_EOF: &str = "stranger-eof";

/// The go signal the test publishes once the publication has returned: the
/// stranger reads the pipe only after it, so the write-end observation is
/// bound to the return boundary instead of a fixed sleep past the deadline
/// (#1141 fix-r1 review P2-1).
const STRANGER_GO: &str = "stranger-go";

/// The signal the stranger publishes once it has confirmed it holds the
/// pipe's read end; the tests below wait for it — the panic test so its
/// deliberate panic lands once the stranger provably exists but no
/// publication step has run yet (#1141 fix-r2 review P1-1), the dead-helper
/// test so neither the helper's death nor the writer's first write can
/// precede the stranger holding the read end (#1141 fix-r3, Linux cb8).
/// Its content is the stranger's own pid (#1155 r3 review P2-2): the
/// group-mode guards below reap the stranger through that pid and never
/// signal through it — the group kill already did — while the Linux
/// pid-mode guard (whose helper runs no group) ends its stranger through
/// the pid the helper publishes instead.
const STRANGER_STARTED: &str = "stranger-started";

/// #1155 r4 review P2: the marker the helper writes once its bounded
/// started-wait observed the stranger's `started` signal — the same cap the
/// test-side wait derives from — so "the helper waited out the handshake
/// before dying" is an assertion, not an assumption.
const STRANGER_SAW: &str = "stranger-saw";

/// The panic the `spawned` hook of the panic test below raises once the
/// stranger has started; comparing the caught payload against it keeps a
/// hook that failed earlier — a stranger that never signalled — from passing
/// as the deliberate window panic (#1141 fix-r2 review P1-1).
const DELIBERATE_PANIC: &str = "#1141 deliberate panic while the stranger is running";

/// The stranger the dead-helper tests leave holding the pipe's read end
/// (#1141, #1125 r4 review P2-1): its first act is to confirm it really
/// holds the pipe — `fstat(0)` must show a FIFO, which is only true when
/// the shell handed the background command the read end instead of the
/// async-rule `/dev/null` (dash, the Linux `/bin/sh`, points a background
/// command's stdin there even behind an explicit `<&0`; bash keeps the
/// pipe — the helper below saves the read end to fd 3 first and redirects
/// from there, which holds under both) — and only then does it publish the
/// `started` signal the helper and the test's `spawned` hook both wait for
/// (#1141 fix-r3, Linux cb8: with the read end lost, the writer died on
/// EPIPE instead of the deadline). A stdin that is not the pipe is
/// recorded as the marker verdict `stdin-not-a-pipe` — the r2 probe read a
/// `/dev/null` stdin as a 0-byte `eof`, silently misclassifying the very
/// broken shape it was there to observe. The `started` signal carries the
/// stranger's own pid (#1155 r3 review P2-2), published atomically (#166
/// r8) after a caller-chosen delay — the dead-helper test delays it past
/// the 5 s a reverted helper's wait would hardcode alone, so a cap that
/// drifts apart again shows up as a helper that gives up first (#1155 r4
/// review P2). After the go signal it drains the pipe with a zero-timeout
/// `select` on its stdin and classifies what it sees — a 0-byte read means
/// EOF was pending, i.e. every write end was closed before the observation,
/// while a not-ready read end means one is still held (the writer is gone
/// by then, so no data can ever arrive and make it ready; the state is
/// monotone, which is what replaces a timing grace). It publishes every
/// verdict — and the `started` signal — atomically through a staging file +
/// rename (#166 r8), and then parks in a bounded sleep that only the test's
/// [`StrangerGuard`] may end, never EOF or a self-chosen lifetime.
/// `/bin/sh` itself cannot make a non-blocking read, so the stranger is
/// this `python3` probe (`python3` is an established dependency of the
/// gateway host runtime and of tests: `host/connection.rs`,
/// `pilotdeck-yaml/tests/resource_scaling.rs`); the script contains no
/// single quotes, so it embeds verbatim into the helper's single-quoted
/// `-c` argument below.
const STRANGER_PROBE: &str = r#"
import os, select, stat, sys, time
marker, go, started, delay_ms = sys.argv[1], sys.argv[2], sys.argv[3], sys.argv[4]
def publish(path, text):
    staging = path + ".staging"
    with open(staging, "w") as handle:
        handle.write(text)
    os.rename(staging, path)
if not stat.S_ISFIFO(os.fstat(0).st_mode):
    publish(marker, "stdin-not-a-pipe")
else:
    time.sleep(int(delay_ms) / 1000.0)
    publish(started, str(os.getpid()))
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
        // #1155 r3 review P2-1: the return-boundary proof holds on every
        // path — success included.
        assert_no_write_end_survives_the_return,
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
        assert_no_write_end_survives_the_return,
    )
    .expect_err("#1125 a helper that never reads must fail its publication");
    assert!(
        error.kind() == io::ErrorKind::TimedOut,
        "#1125 deadline error must be TimedOut: {error}"
    );
    assert_killed_and_reaped(&kill_reported);
    assert_only_the_owner_token_remains(&scratch);
}

/// #1141 (#1125 r4 review P2-1): the helper is already dead — it `exit 0`s
/// on its own once the stranger has confirmed it holds the pipe's read end
/// (`started`), which both the helper's bounded wait and this test's
/// `spawned` hook gate on (#1141 fix-r3, Linux cb8) — while the stranger
/// process still holds the pipe's read end, so neither the helper's death
/// nor its kill can end the blocked write; only the writer's own deadline
/// does. The observable chain: the reap report shows an ordinary exit (a
/// guard-killed helper would report SIGKILL), the write fails with
/// `TimedOut` (a read end closed at the deadline would have surfaced as a
/// broken pipe instead), the helper left its `saw` marker behind — it
/// waited out the handshake before dying, under the one cap both sides
/// derive from (#1155 r4 review P2) — and the write phase measured from
/// the hook's return lasted the full deadline and returned within its
/// grace (#1155 r4 review P2: the deadline starts after the handshake, so
/// the 8 s `started` delay below no longer eats the write budget — and a
/// deadline that starts too early gives up on it and lands under the
/// floor). #1155 r3 review P2-1 replaces the old evidence — the stranger
/// observing the read end after the go signal — with the publication's own
/// return-boundary scan: a write end of the pipe still held by anyone in
/// the process tree at the return fails the publication there, with the
/// scan's error taking precedence over the `TimedOut` the test expects
/// (the stranger's verdict below remains as defense in depth, and the
/// stranger itself — killed in its group and reaped by the test, never
/// left for PID 1, #1155 r3 review P2-2 — records it after the fact).
#[test]
fn a_dead_helper_with_a_stranger_reader_still_closes_the_write_end() {
    let scratch = Scratch::new("helper-dead-stranger");
    let marker = scratch.path().join(STRANGER_EOF);
    // The go, started, and saw signals are test machinery rather than
    // observations of the publication, so they live in a scratch of their
    // own and the residue assertion keeps watching the publication's names.
    let signals = Scratch::new("helper-dead-stranger-signals");
    let go = signals.path().join(STRANGER_GO);
    let started = signals.path().join(STRANGER_STARTED);
    let saw = signals.path().join(STRANGER_SAW);
    // Declared before the publication so every later exit of this test —
    // including a panic from inside the shared helper — unwinds through the
    // guard's Drop (#1141 fix-r1 review P1-1).
    let mut stranger = StrangerGuard::new();
    let (kill_sent, kill_reported) = mpsc::channel();
    let body = "\n".repeat(256 * 1024);
    let deadline = Duration::from_millis(1500);
    let write_began = Cell::new(None::<Instant>);
    let error = write_executable_bounded(
        &scratch.path().join("script"),
        &body,
        0o755,
        deadline,
        |_| {
            dead_helper_with_a_stranger_reader(&marker, &go, &started, &saw, STRANGER_STARTED_DELAY)
        },
        // #1141 fix-r2 review P1-1: the helper runs as its own group leader
        // (`process_group(0)` below), so its spawn-returned id is the group
        // the stranger lands in — adopted here, before the helper or the
        // stranger can do anything at all. #1141 fix-r3 (Linux cb8): the
        // stranger publishes `started` only once its stdin provably is the
        // pipe, so waiting here keeps the writer from starting — and the
        // helper below from dying — before the read end is held; with the
        // read end lost (dash points a background stdin at /dev/null), the
        // writer died on EPIPE instead of the deadline. #1155 r4 review P2:
        // the wait below runs inside this hook, and the write deadline
        // starts only after it, so the handshake cannot consume the write
        // budget — which is why the hook's return is where the write phase
        // begins (`write_began`).
        |pid| {
            stranger.adopt(helper_group(pid));
            let started_pid = wait_for_signal(&started, STRANGER_STARTED_TIMEOUT).expect(
                "#1141 the stranger must confirm it holds the pipe read end \
                 before the publication writes",
            );
            stranger.watch(stranger_pid(&started_pid));
            write_began.set(Some(Instant::now()));
        },
        |status| {
            let _ = kill_sent.send(status);
        },
        assert_no_write_end_survives_the_return,
    )
    .expect_err("#1141 a dead helper with a stranger reader must fail its publication");
    let write_began = write_began
        .get()
        .expect("#1155 the spawned hook must record where the write phase began");
    let took = write_began.elapsed();
    assert!(
        took + DEADLINE_FLOOR_SLACK >= deadline,
        "#1155 the write phase must last its full deadline: {took:?} elapsed"
    );
    assert!(
        took < deadline + RETURN_GRACE,
        "#1155 the publication must return within its deadline: {took:?} elapsed"
    );
    assert!(
        error.kind() == io::ErrorKind::TimedOut,
        "#1141 deadline error must be TimedOut: {error}"
    );
    assert!(
        saw.exists(),
        "#1155 the helper must observe the stranger's started signal before it dies"
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
    // verdict, not a timestamp difference, is the judgment. #1155 r3 review
    // P2-1 demoted it from the only evidence to the second witness: the
    // publication's own return-boundary scan already failed any write end
    // still held at the return — which also makes this the failure point a
    // leaked write end reaches on hosts without `/proc`, where the scan
    // itself does not run.
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
    let saw = signals.path().join(STRANGER_SAW);
    let mut stranger = StrangerGuard::new();
    let (kill_sent, _kill_reported) = mpsc::channel();
    let body = "\n".repeat(256 * 1024);
    let publication = catch_unwind(AssertUnwindSafe(|| {
        write_executable_bounded(
            &scratch.path().join("script"),
            &body,
            0o755,
            Duration::from_millis(1500),
            |_| dead_helper_with_a_stranger_reader(&marker, &go, &started, &saw, Duration::ZERO),
            |pid| {
                stranger.adopt(helper_group(pid));
                let started_pid = wait_for_signal(&started, STRANGER_STARTED_TIMEOUT)
                    .expect("#1141 the stranger must signal that it started");
                stranger.watch(stranger_pid(&started_pid));
                panic!("{DELIBERATE_PANIC}");
            },
            |status| {
                let _ = kill_sent.send(status);
            },
            // The panic below unwinds before a writer ever exists, so there
            // is no return boundary to prove here.
            |_| Ok(()),
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

/// `/bin/sh` saves the pipe's read end to fd 3, launches the stranger from
/// there, and `exit 0`s once the stranger has confirmed it holds the read
/// end, so the helper is dead long before the deadline while the stranger
/// keeps holding the pipe's read end (#1141). The fd 3 detour is the
/// load-bearing part: the POSIX async rule points a background command's
/// stdin at `/dev/null`, and dash (the Linux `/bin/sh`) applies it even
/// behind an explicit `<&0` — with the read end lost there, the writer died
/// on a broken pipe instead of its deadline (Linux cb8, #1141 fix-r3) —
/// while saving to fd 3 first and redirecting with `<&3` keeps the pipe
/// under both dash and bash (this machine, 10/10 each: `<&0 &` keeps fd 0 a
/// FIFO under bash only; `exec 3<&0` + `<&3 &` under both). The bounded
/// started-wait — never an unbounded `wait`, so a stranger that never starts
/// cannot wedge the helper past its own deadline — is the other half of the
/// handshake: the helper may not die before the stranger provably holds the
/// read end, and it writes the `saw` marker once it has waited the handshake
/// out (#1155 r4 review P2), under the one cap both sides derive from —
/// counted on the wall clock, because a round count is only an upper bound
/// on time under load (#1155 fix-r1 review: the negative control re-hardcodes
/// 5 s, and a stretched round count could out-wait the delayed `started`).
/// The helper runs as its own process-group leader (`process_group(0)`), so
/// the stranger — and everything it runs — stays in a group the test alone
/// can end, and whose id is the spawn-returned one the `spawned` hook hands
/// over: the group exists from the spawn on, so it never depends on the
/// helper (or the stranger) publishing anything (#1141 fix-r2 review P1-1).
/// The stranger itself is the [`STRANGER_PROBE`] python process: it waits
/// for the go signal the test publishes once the publication has returned
/// (capped, so even a test that never signals leaves it bounded), observes
/// the pipe, records the verdict, and then parks in a bounded sleep that
/// only the test's [`StrangerGuard`] may end, never EOF or a self-chosen
/// lifetime (#1141 fix-r1 review P1-1).
fn dead_helper_with_a_stranger_reader(
    marker: &Path,
    go: &Path,
    started: &Path,
    saw: &Path,
    delay: Duration,
) -> process::Command {
    let delay_ms = delay.as_millis();
    let wait_secs = stranger_started_wait_secs();
    let mut helper = process::Command::new("/bin/sh");
    helper
        .arg("-c")
        .arg(format!(
            "exec 3<&0
               python3 -c '{STRANGER_PROBE}' \"$1\" \"$2\" \"$3\" {delay_ms} <&3 &
               end=$(( $(date +%s) + {wait_secs} ))
               while [ ! -e \"$3\" ] && [ \"$(date +%s)\" -lt \"$end\" ]; do /bin/sleep 0.05; done
               if [ -e \"$3\" ]; then printf saw >\"$4.staging\"; mv \"$4.staging\" \"$4\"; fi
               exit 0"
        ))
        .arg("dead-helper")
        .arg(marker)
        .arg(go)
        .arg(started)
        .arg(saw)
        .stdin(process::Stdio::piped())
        .process_group(0);
    helper
}

/// The helper's bounded started-wait budget, in wall-clock seconds, derived
/// from the same [`STRANGER_STARTED_TIMEOUT`] the test-side wait uses
/// (#1155 r4 review P2): one cap, not two to drift apart — the r4 review
/// found the helper hardcoded at 5 s against the test's 10 s, so the
/// helper could give up on a stranger the test was still willing to wait
/// for.
fn stranger_started_wait_secs() -> u64 {
    STRANGER_STARTED_TIMEOUT.as_secs()
}

/// The helper's spawn-returned id, which `process_group(0)` above also made
/// the process-group id the stranger lands in (#1141 fix-r2 review P1-1).
fn helper_group(id: u32) -> Pid {
    Pid::from_raw(id as i32).expect("#1141 the helper's id must be a valid group id")
}

/// The stranger's pid, published inside its `started` signal (#1155 r3
/// review P2-2): the group-mode guards below use it only to collect the
/// stranger — never to signal it, which the group kill already did. The
/// one exception is the Linux pid-mode guard, whose helper runs no group
/// and whose stranger is therefore both killed and collected through the
/// pid the helper publishes (see [`pid_mode`]).
fn stranger_pid(started: &str) -> Pid {
    let pid: i32 = started
        .trim()
        .parse()
        .expect("#1155 the stranger's started signal must carry its pid");
    Pid::from_raw(pid).expect("#1155 the stranger's pid must be valid")
}

/// Ends the stranger the dead-helper tests leave behind, on every exit of
/// those tests (#1141 fix-r1 review P1-1, #1141 fix-r2 review P1-1). The
/// group id is handed over by the `spawned` hook the moment the helper
/// exists, so the guard is armed before the stranger can even be forked: a
/// helper that dies before forking it, a group file that is never written,
/// an in-flight publication that panics — the guard already holds the group,
/// and no group and no pid at all means the spawn never succeeded or the
/// helper published nothing, so there is nothing to end. #1155 r3 review
/// P2-2: the guard also reaps the stranger itself — this process is a
/// child subreaper for the guard's lifetime, so the orphaned stranger is
/// its child — instead of leaving the collection to PID 1; on Linux an
/// `ECHILD` from that wait is an error, so "reaped here, never by init" is
/// itself the tested invariant.
struct StrangerGuard {
    group: Option<Pid>,
    stranger: Option<Pid>,
    adopted: bool,
    ended: bool,
}

impl StrangerGuard {
    fn new() -> Self {
        Self {
            group: None,
            stranger: None,
            adopted: false,
            ended: false,
        }
    }

    /// Take over the stranger's group at the spawn, through the `spawned`
    /// hook (#1141 fix-r2 review P1-1). The subreaper lease goes up first —
    /// the helper may die at any moment after this, and the orphaned
    /// stranger must land on this process, not on PID 1 (#1155 r3 review
    /// P2-2).
    fn adopt(&mut self, group: Pid) {
        self.acquire_reaper();
        self.group = Some(group);
    }

    /// Record the stranger's pid, published inside its `started` signal, for
    /// the guard's own reaping (#1155 r3 review P2-2).
    fn watch(&mut self, stranger: Pid) {
        self.stranger = Some(stranger);
    }

    fn acquire_reaper(&mut self) {
        adopt_subreaper().expect("#1155 the test process must become its strangers' subreaper");
        self.adopted = true;
    }

    /// The owned group, if the spawn handed one over; the tests assert this
    /// so a regression that never adopts cannot pass vacuously (#1141).
    fn owns_group(&self) -> Option<Pid> {
        self.group
    }

    /// Kill the stranger's whole process group, confirm the orphaned
    /// stranger was reparented here, collect the group's dead, and confirm
    /// the group is empty. The group holds only the dead helper's
    /// descendants, and it cannot have been recycled while the stranger is
    /// still parked in it. The collection comes first: under the subreaper
    /// lease the zombies are this process's children, so the group-empty
    /// poll can only pass after the guard has collected them — there is no
    /// PID 1 left to do it (#1155 r3 review P2-2). A guard whose helper ran
    /// no group delegates to [`pid_mode`], which only exists on Linux.
    fn end(&mut self) -> io::Result<()> {
        if self.ended {
            return Ok(());
        }
        self.ended = true;
        let ended = self.terminate();
        if self.adopted {
            release_subreaper();
            self.adopted = false;
        }
        ended
    }

    fn terminate(&mut self) -> io::Result<()> {
        if let Some(group) = self.group {
            let _ = rustix::process::kill_process_group(group, Signal::KILL);
            self.confirm_reparented()?;
            let deadline = Instant::now() + STRANGER_REAP_TIMEOUT;
            while group_exists(group) && Instant::now() < deadline {
                self.reap_group(group);
                thread::sleep(Duration::from_millis(5));
            }
            if group_exists(group) {
                return Err(io::Error::other(
                    "#1141 the stranger's process group survived the guard's kill",
                ));
            }
            Ok(())
        } else {
            pid_mode::end_stranger(self)
        }
    }

    /// Confirm the orphaned stranger really is this process's child
    /// (#1155 r3 review P2-2): the subreaper lease is what reparents it here
    /// — making the guard, not PID 1, its reaper — so on Linux a wait that
    /// reports `ECHILD` (never reparented, still init's to collect) is an
    /// error rather than a fallthrough, and "the test owns the stranger" is
    /// itself the tested invariant. The observation is a `waitid` with
    /// `NOWAIT`, so it confirms the exit without collecting the zombie:
    /// the group reaping below owns collection, and that split keeps
    /// "was it ours" and "did we collect it" separately breakable. Other
    /// unix kernels have no `PR_SET_CHILD_SUBREAPER` (rustix gates its
    /// prctl module to linux), so the lease never went up there and init
    /// remains the collector.
    fn confirm_reparented(&self) -> io::Result<()> {
        let Some(stranger) = self.stranger else {
            return Ok(());
        };
        #[cfg(target_os = "linux")]
        {
            let deadline = Instant::now() + STRANGER_PID_REAP_TIMEOUT;
            loop {
                match rustix::process::waitid(
                    rustix::process::WaitId::Pid(stranger),
                    rustix::process::WaitIdOptions::EXITED
                        | rustix::process::WaitIdOptions::NOHANG
                        | rustix::process::WaitIdOptions::NOWAIT,
                ) {
                    Ok(Some(_)) => return Ok(()),
                    Ok(None) if Instant::now() < deadline => {
                        thread::sleep(Duration::from_millis(5))
                    }
                    Ok(None) => {
                        return Err(io::Error::other(
                            "#1155 the stranger did not die by the guard's kill",
                        ));
                    }
                    Err(error) => {
                        return Err(io::Error::other(format!(
                            "#1155 the stranger was not reparented to the test's guard: {error}"
                        )));
                    }
                }
            }
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = stranger;
            Ok(())
        }
    }

    /// Collect every exited member of the group (#1155 r3 review P2-2):
    /// under the subreaper lease the test — not PID 1 — is the reaper of
    /// everything the dead helper left in its group, which is not just the
    /// stranger but also the short-lived `/bin/sleep 0.05` of the helper's
    /// started-poll, orphaned whenever a guard kill lands mid-poll. Without
    /// this there is no init fallback under the lease, so a zombie of
    /// either kind would keep the group non-empty forever. Non-Linux
    /// targets run no lease, so init remains the collector there.
    fn reap_group(&self, group: Pid) {
        #[cfg(target_os = "linux")]
        while let Ok(Some(_)) =
            rustix::process::waitpgid(group, rustix::process::WaitOptions::NOHANG)
        {}
        #[cfg(not(target_os = "linux"))]
        let _ = group;
    }
}

/// The pid-mode half of [`StrangerGuard`]: the stranger of a helper that
/// runs no process group of its own can only be ended through its pid
/// (#1155 r3 review P2-2). Linux-only, deliberately (#1155 fix-r1 review
/// P2-1): the pid is read back from the file the helper published and
/// signalled with SIGKILL, which is only safe under the subreaper lease —
/// an unreaped zombie cannot have its pid reused — and only Linux has
/// `PR_SET_CHILD_SUBREAPER`. Off Linux there is no such pin: a stranger
/// that already exited and was collected by init leaves the pid free for
/// reuse, and a kill through a recycled pid could hit an unrelated process,
/// so the pid-mode test does not run there at all and the fallthrough below
/// is an explicit no-op.
#[cfg(target_os = "linux")]
mod pid_mode {
    use super::*;

    /// #1155 r3 review P2-2: the pid file the sleep-5 helper publishes its
    /// stranger under; that helper runs no group of its own, so the guard
    /// can only end its stranger by pid.
    const STRANGER_PID: &str = "stranger-pid";

    /// A stranger process — not the helper — can outlive the helper's kill
    /// while holding the pipe's read end, so killing the helper cannot break
    /// the writer out of its blocked write. Only the writer's own deadline
    /// ends the write and drops the parent's copy of the write end before the
    /// publication returns (#1125 fix-r3 review P2-2). The stranger's `<&3`
    /// off the helper's fd 3 save is what keeps it on the read end: without an
    /// explicit redirection `/bin/sh` points a background command's stdin at
    /// `/dev/null`, and dash does so even with one (`<&0`), so the read end
    /// would close with the helper (#1141, #1125 r4 review P2-1, #1141 fix-r3
    /// Linux cb8 finding). #1155 r3 review P2-2: the helper publishes the
    /// stranger's pid atomically (staging + rename, #166 r8), the `spawned`
    /// hook waits for it and takes a subreaper lease before the helper — the
    /// stranger's original parent — can die, and the [`StrangerGuard`] then
    /// kills and reaps the stranger by pid: this helper runs no group of its
    /// own, so a group kill is not an option, and the collection is the
    /// test's own job, never PID 1's.
    #[test]
    fn a_stranger_on_the_read_end_still_hits_the_write_deadline() {
        let scratch = Scratch::new("helper-stall-stranger");
        let signals = Scratch::new("helper-stall-stranger-signals");
        let pid_file = signals.path().join(STRANGER_PID);
        let mut stranger = StrangerGuard::new();
        let (kill_sent, kill_reported) = mpsc::channel();
        let body = "\n".repeat(256 * 1024);
        let error = write_executable_bounded(
            &scratch.path().join("script"),
            &body,
            0o755,
            Duration::from_millis(1500),
            |_| stalled_behind_a_stranger_reader(&pid_file),
            // The subreaper lease goes up before the publication writes, so the
            // helper's death — whenever it comes — leaves the stranger on this
            // process, and the guard can collect it.
            |_| {
                stranger.follow(wait_for_stranger_pid(&pid_file));
            },
            |status| {
                let _ = kill_sent.send(status);
            },
            assert_no_write_end_survives_the_return,
        )
        .expect_err("#1125 a stranger on the read end must fail its publication");
        assert!(
            error.kind() == io::ErrorKind::TimedOut,
            "#1125 deadline error must be TimedOut: {error}"
        );
        assert_killed_and_reaped(&kill_reported);
        stranger.end().expect(
            "#1155 the stranger must be killed and reaped by the test's guard, not PID 1's",
        );
        assert_only_the_owner_token_remains(&scratch);
    }

    /// `/bin/sh` leaves a background `sleep 5` holding the pipe's read end, then
    /// `exec`s its own sleep: killing the helper cannot close the read end, so
    /// only the writer's deadline ends the write. The stranger's `<&3` off the
    /// helper's fd 3 save is what keeps it on the read end (#1141 fix-r3,
    /// Linux cb8): the POSIX async rule points a background command's stdin at
    /// `/dev/null` — dash applies it even behind an explicit `<&0`; bash honors
    /// the redirect — so the fd 3 save is the shape that holds under both.
    /// #1155 r3 review P2-2: the helper also publishes the stranger's pid —
    /// `$!`, atomically through a staging file + rename (#166 r8) — because
    /// this helper runs no group of its own, so the test's [`StrangerGuard`]
    /// can only end the stranger by pid, and does its own reaping: the
    /// alternative was leaving a zombie (once the helper dies first) to PID 1.
    fn stalled_behind_a_stranger_reader(pid_file: &Path) -> process::Command {
        let staging = pid_file.with_extension("staging");
        let mut helper = process::Command::new("/bin/sh");
        helper
            .arg("-c")
            .arg(format!(
                "exec 3<&0; sleep 5 <&3 & \
                 echo $! >\"{staging}\"; mv \"{staging}\" \"{pid}\"; exec /bin/sleep 30",
                staging = staging.display(),
                pid = pid_file.display(),
            ))
            .arg("stalled-helper")
            .stdin(process::Stdio::piped());
        helper
    }

    /// The pid the sleep-5 helper publishes for its stranger (#1155 r3
    /// review P2-2), waited for by the `spawned` hook so the guard owns the
    /// stranger before the publication continues; the helper renames the
    /// staging file into place (#166 r8), so a partial file is never mistaken
    /// for one.
    fn wait_for_stranger_pid(pid_file: &Path) -> Pid {
        let deadline = Instant::now() + STRANGER_STARTED_TIMEOUT;
        while !pid_file.exists() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(5));
        }
        let published =
            fs::read_to_string(pid_file).expect("#1155 the helper must publish its stranger's pid");
        stranger_pid(&published)
    }

    impl StrangerGuard {
        /// The pid-only counterpart of [`StrangerGuard::adopt`] for a helper
        /// that runs no group of its own (#1155 r3 review P2-2): the
        /// stranger's pid is published by the helper, and the subreaper
        /// lease must be up before the helper — and with it the stranger's
        /// original parent — can die.
        fn follow(&mut self, stranger: Pid) {
            self.acquire_reaper();
            self.stranger = Some(stranger);
        }

        /// The pid-mode counterpart of [`StrangerGuard::reap_group`] for a
        /// helper that runs no group of its own (#1155 r3 review P2-2): the
        /// stranger is this process's child under the lease, so `waitpid`
        /// on its pid alone collects it without touching any other child
        /// of the test.
        fn reap_process(&self, stranger: Pid) -> io::Result<()> {
            let deadline = Instant::now() + STRANGER_PID_REAP_TIMEOUT;
            loop {
                match rustix::process::waitpid(Some(stranger), rustix::process::WaitOptions::NOHANG)
                {
                    Ok(Some(_)) => return Ok(()),
                    Ok(None) if Instant::now() < deadline => {
                        thread::sleep(Duration::from_millis(5))
                    }
                    Ok(None) => {
                        return Err(io::Error::other(
                            "#1155 the stranger was not reaped by the test's guard",
                        ));
                    }
                    Err(error) => {
                        return Err(io::Error::other(format!(
                            "#1155 the stranger was not reparented to the test's guard: {error}"
                        )));
                    }
                }
            }
        }
    }

    /// Kill and reap the stranger by its published pid. The kill's error is
    /// propagated, not swallowed (#1155 fix-r1 review P2-1): under the lease
    /// the stranger's zombie is this process's own child and pins its pid,
    /// so a kill that cannot even be sent is a real anomaly — never a
    /// vacuous pass.
    pub(super) fn end_stranger(guard: &mut StrangerGuard) -> io::Result<()> {
        let Some(stranger) = guard.stranger else {
            return Ok(());
        };
        rustix::process::kill_process(stranger, Signal::KILL).map_err(|error| {
            io::Error::other(format!(
                "#1155 the guard's stranger kill could not be sent: {error}"
            ))
        })?;
        guard.confirm_reparented()?;
        guard.reap_process(stranger)
    }
}

/// Off Linux no pid-mode guard exists (see the module above): every guard
/// that runs here owns a group, so [`StrangerGuard::terminate`] never falls
/// through to this — an explicit no-op, not a silent pass.
#[cfg(not(target_os = "linux"))]
mod pid_mode {
    pub(super) fn end_stranger(_guard: &mut super::StrangerGuard) -> std::io::Result<()> {
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

/// #1155 r3 review P2-2: this process's child-subreaper flag, refcounted
/// because libtest runs these tests on a thread pool and two stranger
/// guards may overlap: the first adoption saves the previous value and
/// raises the flag, later adoptions join it, and the last release restores
/// the saved value. The flag is process-wide, so the count — not the flag
/// itself — decides who restores what.
#[cfg(target_os = "linux")]
static SUBREAPER_ADOPTIONS: Mutex<Option<(Option<Pid>, usize)>> = Mutex::new(None);

/// Become a child subreaper (Linux): orphaned descendants — the stranger,
/// whose parent helper dies before it does — are reparented to this process
/// instead of PID 1, so the test is their reaper (#1155 r3 review P2-2).
/// `PR_SET_CHILD_SUBREAPER` does not exist on other unix kernels (rustix
/// gates its prctl module to linux), so elsewhere this is a no-op and the
/// guards keep the old contract: init collects the orphan, and the
/// group-empty confirmation proves it went.
fn adopt_subreaper() -> io::Result<()> {
    #[cfg(target_os = "linux")]
    {
        let mut adoptions = SUBREAPER_ADOPTIONS
            .lock()
            .expect("#1155 the subreaper adoption state must not be poisoned");
        let (saved, count) = adoptions.get_or_insert_with(|| (None, 0));
        if *count == 0 {
            *saved = rustix::process::child_subreaper()?;
            let mine = Pid::from_raw(process::id() as i32)
                .expect("#1155 this process's id must be a valid pid");
            rustix::process::set_child_subreaper(Some(mine))?;
        }
        *count += 1;
    }
    #[cfg(not(target_os = "linux"))]
    let _ = ();
    Ok(())
}

/// Step the subreaper refcount back down, restoring the saved flag value on
/// the last release (#1155 r3 review P2-2). Best effort on purpose: a
/// restore that fails only leaves this process a subreaper, which harms
/// nothing outside this test run.
fn release_subreaper() {
    #[cfg(target_os = "linux")]
    {
        let Ok(mut adoptions) = SUBREAPER_ADOPTIONS.lock() else {
            return;
        };
        let Some((_saved, count)) = adoptions.as_mut() else {
            return;
        };
        *count -= 1;
        if *count == 0 {
            let (saved, _) = adoptions
                .take()
                .expect("#1155 the subreaper adoption state must be present");
            let _ = rustix::process::set_child_subreaper(saved);
        }
    }
}

/// Whether the group still has at least one member, zombies included —
/// the lib's `process_group::group_exists`, mirrored here because the
/// lib's test-support module is not visible across the integration-crate
/// boundary.
fn group_exists(group: Pid) -> bool {
    rustix::process::test_kill_process_group(group).is_ok()
}

/// #1155 r3 review P2-1: the return-boundary proof. The scan runs inside
/// the publication — after `published` is decided, before it returns — so
/// there is no gap between "the last write end closed" and "someone
/// checked" for a copy released in between to hide in, which is exactly the
/// gap the old read-end observation left. Soundness: a copy held at the
/// return moment is held through the whole scan — holding is monotone, only
/// a release makes an fd disappear — so the scan enumerates it; a copy
/// released mid-scan does not violate "closed at return"; and no new copy of
/// this pipe's write end can appear, because the writer thread — whose
/// `ChildStdin` was this process's only source of the descriptor — has
/// already been asserted finished, while no other process can acquire the
/// descriptor: only this process's descendants can inherit it, and any copy
/// they already hold is precisely what the scan must find — with one
/// exception, a pre-exec transient: a fork of this process that has not
/// exec'd yet holds an inherited CLOEXEC copy for its fork-to-exec window
/// (the same window #1125 was about) and is on its way out, not a leak, so
/// a candidate must persist through [`TRANSIENT_WRITE_END_GRACE`] before
/// it is declared a survivor. Directories
/// that cannot be read belong to a process that is dying — it holds
/// nothing — or to another user, which can never hold this pipe, so
/// unreadable entries are skipped. An fd of the pipe whose fdinfo cannot
/// be read, or whose flags line cannot be parsed, is an unknown rather
/// than a read end: only the fd's own disappearance (ENOENT between the
/// readlink and the read) proves the copy was released, so every other
/// read failure fails the scan instead of guessing the fd harmless
/// (#1155 fix-r1 review, opus P2). The pipe is identified by its inode —
/// both ends share it and no live pipe reuses it — and a write end by its
/// fdinfo access mode (`O_WRONLY`/`O_RDWR`, the low two flag bits); a read
/// end (mode `0`) is the stranger, and passes. `/proc` is Linux: elsewhere
/// the proof is skipped and the stranger's marker observation remains the
/// evidence.
fn assert_no_write_end_survives_the_return(pipe: u64) -> io::Result<()> {
    #[cfg(target_os = "linux")]
    {
        let wanted = format!("pipe:[{pipe}]");
        let mut candidates: Vec<String> = Vec::new();
        let processes = fs::read_dir("/proc").map_err(|error| {
            io::Error::other(format!(
                "#1155 reading /proc for the return-boundary scan: {error}"
            ))
        })?;
        for process in processes {
            let process = process
                .map_err(|error| io::Error::other(format!("#1155 reading /proc: {error}")))?;
            let pid = process.file_name().to_string_lossy().into_owned();
            if !pid.bytes().all(|byte| byte.is_ascii_digit()) {
                continue;
            }
            let descriptors = match fs::read_dir(format!("/proc/{pid}/fd")) {
                Ok(descriptors) => descriptors,
                Err(_) => continue,
            };
            for descriptor in descriptors {
                let Ok(descriptor) = descriptor else {
                    continue;
                };
                let Ok(target) = fs::read_link(descriptor.path()) else {
                    continue;
                };
                if target.to_string_lossy() != wanted {
                    continue;
                }
                let number = descriptor.file_name();
                let fdinfo = match fs::read_to_string(format!(
                    "/proc/{pid}/fdinfo/{}",
                    number.to_string_lossy()
                )) {
                    Ok(fdinfo) => fdinfo,
                    // The fd disappeared between the readlink and this
                    // read: the copy was released, which is what the scan
                    // is about to require of it anyway.
                    Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                    Err(error) => {
                        return Err(io::Error::other(format!(
                            "#1155 reading the fdinfo of /proc/{pid}/fd/{}: {error}",
                            number.to_string_lossy()
                        )));
                    }
                };
                let access = match fdinfo
                    .lines()
                    .find_map(|line| line.strip_prefix("flags:"))
                    .and_then(|flags| u32::from_str_radix(flags.trim(), 8).ok())
                {
                    Some(access) => access,
                    None => {
                        return Err(io::Error::other(format!(
                            "#1155 the fdinfo of /proc/{pid}/fd/{} has no parsable flags line",
                            number.to_string_lossy()
                        )));
                    }
                };
                if access & 0o3 == 0 {
                    // A read end — the stranger holds exactly that.
                    continue;
                }
                candidates.push(format!("/proc/{pid}/fd/{}", number.to_string_lossy()));
            }
        }
        // A candidate counts as a surviving write end only if it persists
        // through the transient grace: the writer thread is finished, so
        // nothing new can acquire the descriptor, while a pre-exec fork of
        // this process loses its inherited copy at the exec already
        // underway.
        let deadline = Instant::now() + TRANSIENT_WRITE_END_GRACE;
        while !candidates.is_empty() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(25));
            candidates.retain(|fd| {
                fs::read_link(fd).is_ok_and(|target| target.to_string_lossy() == wanted)
            });
        }
        if let Some(fd) = candidates.first() {
            return Err(io::Error::other(format!(
                "#1155 a write end of the publication pipe ({fd}) survived the return"
            )));
        }
    }
    #[cfg(not(target_os = "linux"))]
    let _ = pipe;
    Ok(())
}

/// Wait until the stranger has published `signal`, bounded by `wait`; the
/// stranger publishes the signal only once it has confirmed it holds the
/// pipe's read end, so the bound only covers a stranger that never started
/// or never got the read end (#1141 fix-r2 review P1-1, fix-r3). Its
/// content — the stranger's pid — is the caller's to keep (#1155 r3
/// review P2-2).
fn wait_for_signal(signal: &Path, wait: Duration) -> io::Result<String> {
    let deadline = Instant::now() + wait;
    while !signal.exists() && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(5));
    }
    fs::read_to_string(signal)
        .map_err(|_| io::Error::other("#1141 the stranger never signalled that it started"))
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
