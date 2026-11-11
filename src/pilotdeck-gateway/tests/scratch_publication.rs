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
    path::{Path, PathBuf},
    process,
    sync::mpsc::{self, Receiver},
    thread,
    time::{Duration, Instant},
};

use rustix::process::{Pid, Signal};
use std::os::unix::process::CommandExt;

#[cfg(target_os = "linux")]
use std::collections::HashMap;

#[cfg(target_os = "linux")]
use std::os::fd::AsFd;

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
/// #1297 review (sibling P1): that 10 s left the within-cap scenario only
/// `10 s − STRANGER_STARTED_DELAY` = 2 s of headroom for the probe's startup
/// — the same "fixed delay against a cap two seconds away" shape #1297 was
/// filed against, kept in the very file that fixes it. The scenario needs its
/// 8 s delay (> the 5 s a re-hardcoded helper cap would allow), so the cap
/// moves out instead: 14 s, six seconds of headroom for a boot that measures
/// ~15 ms when the host is idle.
const STRANGER_STARTED_TIMEOUT: Duration = Duration::from_secs(14);

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
/// — 8 s, comfortably past the 5 s a reverted helper's wait would hardcode
/// on its own wall clock.
/// A round-counted mutant could stretch past a fixed delay under load
/// (each round is a `/bin/sleep 0.05` plus its fork/exec), so the helper's
/// bounded wait below is wall-clock (`date +%s`), which load cannot
/// stretch: a 5 s cap gives up 3 s of real time before the delayed
/// `started` appears, on any host. #1297: this is scenario pacing now — only
/// the within-cap test below delays its `started` this long (the delay is
/// what exposes a re-hardcoded 5 s cap there). It still has to complete
/// inside the cap, so the cap carries the headroom ([`STRANGER_STARTED_TIMEOUT`]);
/// what the release signal decides is only whether `started` appears at all.
const STRANGER_STARTED_DELAY: Duration = Duration::from_secs(8);

/// #1409 item 6: the two constants' relation was prose only — the delay has
/// to outrun the 5 s a re-hardcoded helper cap would use (that is what the
/// within-cap scenario exposes), and the test-side wait has to keep the
/// headroom the #1297 review asked for (at least four seconds above the
/// delay). `Duration::as_secs` is const, so the invariant is checked at
/// compile time instead of drifting with the comments.
const _: () = assert!(
    STRANGER_STARTED_DELAY.as_secs() > 5
        && STRANGER_STARTED_DELAY.as_secs() + 4 <= STRANGER_STARTED_TIMEOUT.as_secs()
);

/// #1297 review (concurrency P1 + sibling P1): how long the dead-helper
/// test's stranger holds its `started` back after confirming it holds the
/// pipe — deliberate pacing, and the load-bearing number of this fixture.
///
/// What decides when `started` appears is this delay plus the interpreter
/// boot: the test publishes the `release` signal before the probe can even
/// boot (it gates the healthy path — the probe's own wait is capped at 60 s
/// and it publishes `started` even then, a fallback this side's 14 s wait
/// never reaches — but it never actually waits), so the sleep below is not
/// a redundant decoration to be tidied away. The `expiry-hoisted-before-spawn`
/// mutant kills only while the hook provably outlasts
/// [`DEADLINE_FLOOR_SLACK`]: hoisting the expiry above the spawn moves
/// everything the hook spends into the write budget, so the floor assertion
/// catches the mutant by exactly `hook − slack − teardown` (the return
/// path's scan, kill and reap run after the write phase and are inside the
/// measured `took`). The boot alone (~15 ms measured; load can starve it
/// below the 5 ms floor) is not a margin; this constant is. On Linux the
/// teardown also contains the return-boundary `/proc` scan, whose
/// `TRANSIENT_WRITE_END_GRACE` is 500 ms — a candidate write end is given
/// that long to disappear — so the pacing has to sit comfortably above that
/// grace, not merely above the boot. 1 s is ~200× the slack and still 14×
/// inside the 14 s started cap: a delay the test chooses, never a wait on a
/// cap it does not own. Deleting or shrinking it takes the mutant's kill
/// margin with it (#1297 review; follow-up #1636 carries the Linux
/// measurement).
const DEAD_HELPER_STRANGER_DELAY: Duration = Duration::from_millis(1000);

/// How long the helper below may take to record its write-end observation
/// after the go signal: the go decision is made on an arbitrary scheduler and
/// every observation is bounded to complete long before this runs out (#1141
/// fix-r3 review P1-2).
const MARKER_TIMEOUT: Duration = Duration::from_secs(8);

/// #1155 r4 review P2: how far below the deadline the measured write phase
/// may land; a healthy writer cannot give up before its own expiry — the
/// deadline now starts where the `spawned` hook returns — so the floor only
/// absorbs the instant between the hook's return and the expiry computation
/// (microseconds; the writer's give-up check can only overshoot the expiry,
/// never land before it). #1297 tightened the slack from 100 ms to 5 ms
/// together with the event-driven release: the floor is also what kills
/// the expiry-hoisted mutant, and that kill is deterministic only while the
/// handshake outlives the slack plus whatever the return path spends after
/// the write phase (the mutant moves the hook into the write budget, while
/// `took` still includes the scan/kill/reap that follow it). The old
/// 8-second delay covered that by accident; the margin is bought
/// deliberately now — the dead-helper test's stranger holds its `started`
/// back for [`DEAD_HELPER_STRANGER_DELAY`] (1 s), so the kill margin is
/// ~1 s rather than the interpreter boot's ~15 ms, which load could starve
/// below the slack (#1297 review, concurrency P1).
const DEADLINE_FLOOR_SLACK: Duration = Duration::from_millis(5);

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

/// #1155 fix-r3 review P1-1: the receipt the helper publishes — atomically,
/// staging + rename (#166 r8) — once it has computed its `end` deadline,
/// launched the stranger, and reached its paused poll. The tests below wait
/// for it before anything else, so the event ordering of the cross-deadline
/// scenario is confirmed, never slept into: a helper that the scheduler parks
/// between computing `end` and entering its poll can no longer skip the poll
/// round entirely (the old shape's missed-kill window — the paused poll was
/// one fixed 10 s sleep that only *usually* covered both the delayed
/// `started` and the cap).
const STRANGER_PAUSED: &str = "stranger-paused";

/// #1155 fix-r3 review P1-1: the signal the tests publish — atomically, again
/// — to explicitly resume a helper parked at its paused poll. The resumption,
/// not a sleep duration, is what moves the helper on to its saw write, so the
/// moment of that write is by construction after everything the test has
/// already observed: the pause receipt, the stranger's `started`, and (when
/// the scenario needs it) the cap deadline having been crossed.
const STRANGER_RESUME: &str = "stranger-resume";

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
/// r8) behind the `started.release` signal (#1297): the release gates the
/// healthy path — the test owns it and publishes it before the probe boots —
/// while the moment itself is the pacing sleep plus the interpreter boot,
/// since the test publishes the signal before the probe has booted. (The
/// probe's wait is bounded at 60 s and still publishes `started` after that:
/// a fallback that keeps a broken test from hanging, unreachable today
/// because this side's 14 s [`STRANGER_STARTED_TIMEOUT`] fails first —
/// #1297 third-round review, DeepSeek concurrency P3 + MiniMax boundary P3.)
/// That is deliberate: the old 8-second sleep used to
/// race a 10-second cap with only the python startup as headroom, so the
/// pacing is now a number this fixture owns and sizes for the mutants that
/// read it (`DEAD_HELPER_STRANGER_DELAY` for the dead-helper test's kill
/// margin, `STRANGER_STARTED_DELAY` for the within-cap scenario that must
/// outrun a re-hardcoded 5 s cap), never a wait on a cap it does not.
/// After the go signal it drains
/// the pipe with a zero-timeout
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
    release = started + ".release"
    deadline = time.monotonic() + 60.0
    while not os.path.exists(release) and time.monotonic() < deadline:
        time.sleep(0.05)
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
/// the handshake below no longer eats the write budget — and a deadline
/// that starts too early gives up on it and lands under the floor).
/// #1297: the release signal gates the healthy path — the hook publishes it,
/// the stranger (after confirming its stdin is the pipe and its pacing sleep)
/// publishes `started` once that file appears (or after its own 60 s
/// fallback, which this side's 14 s wait never reaches), and the
/// helper's saw write follows the hook's resume. The pacing sleep is what
/// sets the moment (`DEAD_HELPER_STRANGER_DELAY` here), and it exists for
/// the mutants: the old 8-second sleep left only the python startup as
/// headroom inside the then-10-second started cap, which is the flake this
/// test no longer has — the cap now carries six seconds of headroom
/// ([`STRANGER_STARTED_TIMEOUT`]).
/// #1155 r3 review P2-1 replaces the old evidence — the stranger
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
    // The go, started, release, paused, resume, and saw signals are test
    // machinery rather than observations of the publication, so they live in
    // a scratch of their own and the residue assertion keeps watching the
    // publication's names.
    let signals = Scratch::new("helper-dead-stranger-signals");
    let go = signals.path().join(STRANGER_GO);
    let started = signals.path().join(STRANGER_STARTED);
    let release = release_signal(&started);
    let saw = signals.path().join(STRANGER_SAW);
    let paused = signals.path().join(STRANGER_PAUSED);
    let resume = signals.path().join(STRANGER_RESUME);
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
            dead_helper_with_a_stranger_reader(
                &marker,
                &go,
                &started,
                &saw,
                &paused,
                &resume,
                // #1297 review (concurrency P1): the pacing delay below the
                // write deadline is what keeps the expiry-hoisted mutant
                // killable — see [`DEAD_HELPER_STRANGER_DELAY`].
                DEAD_HELPER_STRANGER_DELAY,
            )
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
        // begins (`write_began`). #1155 fix-r3 review P1-1: the helper
        // parks at its paused poll until this hook — the only place that
        // knows the handshake is done — releases it, so the saw write always
        // happens with the `started` signal provably present, never on the
        // far side of a scheduling pause that skipped the helper's poll
        // round outright. #1297: the release signal below gates the healthy
        // path — the test owns that file and publishes it before the probe
        // can boot (a probe that never sees it still publishes `started`
        // after its own 60 s fallback, which this side's 14 s wait never
        // reaches) — so the wait here is bounded polling over an event this
        // test owns. What sets the moment of `started` is the stranger's
        // interpreter boot (~15 ms measured on this host, load only grows
        // it) plus the deliberate [`DEAD_HELPER_STRANGER_DELAY`] pacing the
        // floor assertion needs; neither is a cap this test has to fit
        // inside.
        |pid| {
            stranger.adopt(helper_group(pid));
            publish_release_signal(&release);
            let started_pid = wait_for_signal(&started, STRANGER_STARTED_TIMEOUT).expect(
                "#1141 the stranger must confirm it holds the pipe read end \
                 before the publication writes",
            );
            stranger.watch(stranger_pid(&started_pid));
            publish_resume_signal(&resume);
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

/// #1155 fix-r2b review (Codex P1-1): a `started` that appears after the
/// helper's cap must not be recorded as seen, even when the helper's poll
/// was paused across the deadline so it resumes to find the file already
/// there — the saw write re-validates the same deadline the pause straddled,
/// so the file-exists short-circuit cannot skip it. #1155 fix-r3 review
/// P1-1: the pause is an explicit event barrier, not a load gamble. The
/// helper publishes its `paused` receipt once its `end` deadline is computed
/// and the stranger launched; this test then waits for the stranger's
/// 2-second delayed `started`, proves the same deadline has been crossed
/// (`end` was computed before the receipt, so it is at most the receipt's
/// observation time plus the 1-second cap), and only then resumes the helper
/// through the atomic `resume` signal. The saw write therefore runs with the
/// started file provably present and the deadline provably gone — the exact
/// interleaving the review's missed kill needed, reached by construction
/// instead of by a 10-second sleep that only usually covers a 2-second delay
/// and a 1-second cap.
#[test]
fn a_started_past_the_helper_cap_is_not_seen_after_a_paused_poll() {
    // Every signal here is test machinery, so the whole test lives in a
    // scratch of its own.
    let signals = Scratch::new("helper-late-started-signals");
    let marker = signals.path().join(STRANGER_EOF);
    let go = signals.path().join(STRANGER_GO);
    let started = signals.path().join(STRANGER_STARTED);
    let release = release_signal(&started);
    let saw = signals.path().join(STRANGER_SAW);
    let paused = signals.path().join(STRANGER_PAUSED);
    let resume = signals.path().join(STRANGER_RESUME);
    // Declared before the spawn so every later exit of this test — including
    // a panic from inside the assertions — unwinds through the guard's Drop.
    let mut stranger = StrangerGuard::new();
    // The cap this scenario crosses is deliberately shorter than the
    // stranger's 2-second `started` delay; it is a local parameter of the
    // scenario, not the derived handshake cap of
    // [`a_started_within_the_helper_cap_is_still_seen_after_a_paused_poll`].
    const SCENARIO_CAP: Duration = Duration::from_secs(1);
    let mut helper = started_wait_helper(
        &marker,
        &go,
        &started,
        &saw,
        &paused,
        &resume,
        Duration::from_secs(2),
        SCENARIO_CAP.as_secs(),
    )
    .spawn()
    .expect("#1155 the late-started helper must spawn");
    stranger.adopt(helper_group(helper.id()));
    // #1297: release before the helper's stranger can even boot, so the
    // 2-second scenario delay — not the release — keeps deciding when
    // `started` appears.
    publish_release_signal(&release);
    // The barrier receipt: the helper has computed `end`, launched the
    // stranger, and is now parked at its paused poll.
    wait_for_signal(&paused, STRANGER_STARTED_TIMEOUT)
        .expect("#1155 the late-started helper must reach its paused poll");
    let paused_at = Instant::now();
    // The stranger did publish `started` — the later refusal must be the
    // deadline's, not a stranger that never started.
    let started_pid = wait_for_signal(&started, STRANGER_STARTED_TIMEOUT)
        .expect("#1155 the late-started stranger must still publish its started signal");
    stranger.watch(stranger_pid(&started_pid));
    // The same deadline has been crossed: `end` was computed before the
    // receipt, so `end <= paused_at + SCENARIO_CAP` — waiting that far past
    // the receipt proves the saw write below runs past the deadline. (The
    // observed `started` — no earlier than the helper's own 2-second delay,
    // which outlives the 1-second cap — already implies it; this explicit
    // wait is the belt to that braces, and costs nothing at these constants.)
    let crossed_at = paused_at + SCENARIO_CAP;
    while Instant::now() < crossed_at {
        thread::sleep(Duration::from_millis(5));
    }
    publish_resume_signal(&resume);
    let exited = helper
        .wait()
        .expect("#1155 the late-started helper must exit on its own");
    assert!(
        exited.success(),
        "#1155 the late-started helper must exit cleanly: {exited}"
    );
    assert!(
        !saw.exists(),
        "#1155 a started past the helper's cap must not be recorded as seen after a paused poll"
    );
    stranger
        .end()
        .expect("#1155 the late-started stranger must be killed and reaped by the test's guard");
}

/// #1155 fix-r3 review P1-1: the other side of the same controlled pause —
/// a `started` that appears *within* the helper's cap must still be recorded
/// as seen, and it is the derived cap that makes it so. The helper parks at
/// the same event barrier; this test waits for the stranger's
/// [`STRANGER_STARTED_DELAY`]ed `started` (8 s today — about 57 % of the
/// 14 s [`STRANGER_STARTED_TIMEOUT`], and comfortably past the 5 s a
/// re-hardcoded helper cap would allow) and
/// resumes it immediately, so the saw write runs while the derived cap —
/// [`stranger_started_wait_secs`], the one constant both sides of the
/// handshake derive from — still has budget. A helper whose cap has drifted
/// back down to a hardcoded 5 s finds its deadline already gone at that same
/// moment, refuses the saw write, and this test's saw assertion is what
/// catches it, in the very scenario (`started-cap-hardcoded-short`) rather
/// than in a different test's timing.
#[test]
fn a_started_within_the_helper_cap_is_still_seen_after_a_paused_poll() {
    let signals = Scratch::new("helper-within-cap-signals");
    let marker = signals.path().join(STRANGER_EOF);
    let go = signals.path().join(STRANGER_GO);
    let started = signals.path().join(STRANGER_STARTED);
    let release = release_signal(&started);
    let saw = signals.path().join(STRANGER_SAW);
    let paused = signals.path().join(STRANGER_PAUSED);
    let resume = signals.path().join(STRANGER_RESUME);
    let mut stranger = StrangerGuard::new();
    let mut helper = started_wait_helper(
        &marker,
        &go,
        &started,
        &saw,
        &paused,
        &resume,
        STRANGER_STARTED_DELAY,
        stranger_started_wait_secs(),
    )
    .spawn()
    .expect("#1155 the within-cap helper must spawn");
    stranger.adopt(helper_group(helper.id()));
    // #1297: release before the helper's stranger can even boot, so the
    // scenario's 8-second delay — past the 5 s a re-hardcoded helper cap
    // would allow — keeps deciding when `started` appears.
    publish_release_signal(&release);
    wait_for_signal(&paused, STRANGER_STARTED_TIMEOUT)
        .expect("#1155 the within-cap helper must reach its paused poll");
    let started_pid = wait_for_signal(&started, STRANGER_STARTED_TIMEOUT)
        .expect("#1155 the within-cap stranger must publish its started signal");
    stranger.watch(stranger_pid(&started_pid));
    // Resume right away: the derived cap (15 s) still holds budget at the
    // 8-second `started`, so a healthy helper records the saw, while one
    // whose cap was re-hardcoded to 5 s has already lost its deadline.
    publish_resume_signal(&resume);
    let exited = helper
        .wait()
        .expect("#1155 the within-cap helper must exit on its own");
    assert!(
        exited.success(),
        "#1155 the within-cap helper must exit cleanly: {exited}"
    );
    assert!(
        saw.exists(),
        "#1155 a started within the helper's cap must still be recorded as seen after a paused poll"
    );
    stranger
        .end()
        .expect("#1155 the within-cap stranger must be killed and reaped by the test's guard");
}

/// #1392 (#1155 r4 review P1, P2b follow-up): the barrier's own safety
/// timeout is a failure, never a silent pass. The r4 review's leak: with
/// the test thread and the stranger both starved past the helper's
/// 30-second park, the helper ran on alone, skipped its saw write, and
/// still exited 0 — so a starved run of the `saw-write-skips-the-cap`
/// shape passed both the `exited.success()` and the `!saw.exists()`
/// assertions without any scheduling luck at all. This test reproduces
/// that interleaving by construction: it waits for the `paused` receipt
/// and the stranger's `started`, then deliberately never publishes
/// `resume`, so the helper's park runs out on its own. The `started` file
/// provably exists by then (published seconds into the park) and the
/// scenario cap's `end` provably does not (1 s from the spawn, far under
/// the 30 s park) — exactly the state the starved scenario left the saw
/// write in: file present, deadline gone. The helper must report the
/// timed-out barrier by exiting non-zero — never the `exit 0` that made
/// the leak invisible — and must still refuse the saw write on the far
/// side of its own deadline. A helper that somehow exits early (the test
/// thread itself starved past the whole park) fails the elapsed floor
/// below instead of passing vacuously.
#[test]
fn a_barrier_timeout_without_a_resume_fails_the_helper() {
    let signals = Scratch::new("helper-barrier-timeout-signals");
    let marker = signals.path().join(STRANGER_EOF);
    let go = signals.path().join(STRANGER_GO);
    let started = signals.path().join(STRANGER_STARTED);
    let release = release_signal(&started);
    let saw = signals.path().join(STRANGER_SAW);
    let paused = signals.path().join(STRANGER_PAUSED);
    let resume = signals.path().join(STRANGER_RESUME);
    let mut stranger = StrangerGuard::new();
    // The scenario's local parameters, like the late-started test's: a
    // 2-second `started` delay keeps the signal well inside the park (it
    // has provably existed for ~28 s by the timeout), while the 1-second
    // cap keeps `end` long gone by then — a pairing that leaves a mutant
    // dropping the deadline re-check as the only shape that could write
    // the saw marker on this path.
    const SCENARIO_CAP: Duration = Duration::from_secs(1);
    let mut helper = started_wait_helper(
        &marker,
        &go,
        &started,
        &saw,
        &paused,
        &resume,
        Duration::from_secs(2),
        SCENARIO_CAP.as_secs(),
    )
    .spawn()
    .expect("#1392 the barrier-timeout helper must spawn");
    stranger.adopt(helper_group(helper.id()));
    // #1297: release before the helper's stranger can even boot, so the
    // 2-second scenario delay keeps deciding when `started` appears.
    publish_release_signal(&release);
    wait_for_signal(&paused, STRANGER_STARTED_TIMEOUT)
        .expect("#1392 the barrier-timeout helper must reach its paused poll");
    let started_pid = wait_for_signal(&started, STRANGER_STARTED_TIMEOUT)
        .expect("#1392 the barrier-timeout stranger must publish its started signal");
    stranger.watch(stranger_pid(&started_pid));
    // No resume, deliberately: the park must run out on its own.
    let parked_at = Instant::now();
    let exited = helper
        .wait()
        .expect("#1392 the barrier-timeout helper must exit on its own");
    assert!(
        !exited.success(),
        "#1392 a barrier timed out without a resume must fail the helper, not exit cleanly: {exited}"
    );
    assert_eq!(
        exited.code(),
        Some(3),
        "#1392 the timed-out barrier must be the helper's exit reason: {exited}"
    );
    // The timeout path really ran — the helper parked out the safety span,
    // it did not exit on an instantly received signal. `date +%s`
    // truncation can shave up to a second off the nominal 30 s, and the
    // 2-second `started` delay lands inside the measured span, so the
    // floor sits well below both while still annulling an instant exit.
    assert!(
        parked_at.elapsed() >= Duration::from_secs(25),
        "#1392 the helper must actually park out its timed-out barrier: {:?} elapsed",
        parked_at.elapsed()
    );
    assert!(
        !saw.exists(),
        "#1392 a started past the helper's cap must not be recorded as seen across a timed-out barrier"
    );
    stranger
        .end()
        .expect("#1392 the barrier-timeout stranger must be killed and reaped by the test's guard");
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
    // go, started, paused, and resume signals all live in a scratch of their
    // own and the residue assertion watches the publication's names alone.
    let signals = Scratch::new("helper-panic-stranger-signals");
    let marker = signals.path().join(STRANGER_EOF);
    let go = signals.path().join(STRANGER_GO);
    let started = signals.path().join(STRANGER_STARTED);
    let release = release_signal(&started);
    let saw = signals.path().join(STRANGER_SAW);
    let paused = signals.path().join(STRANGER_PAUSED);
    let resume = signals.path().join(STRANGER_RESUME);
    let mut stranger = StrangerGuard::new();
    let (kill_sent, _kill_reported) = mpsc::channel();
    let body = "\n".repeat(256 * 1024);
    let publication = catch_unwind(AssertUnwindSafe(|| {
        write_executable_bounded(
            &scratch.path().join("script"),
            &body,
            0o755,
            Duration::from_millis(1500),
            |_| {
                dead_helper_with_a_stranger_reader(
                    &marker,
                    &go,
                    &started,
                    &saw,
                    &paused,
                    &resume,
                    Duration::ZERO,
                )
            },
            |pid| {
                stranger.adopt(helper_group(pid));
                // #1297: the release signal triggers the stranger's
                // `started` here too, so the deliberate panic below still
                // lands once the stranger provably holds the read end.
                publish_release_signal(&release);
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
/// #1155 fix-r3 review P1-1: the wait is an explicit pause — the helper
/// publishes its `paused` receipt and parks until the test releases it — so
/// the helper cannot skip its poll round to the far side of a scheduling
/// stall, and the saw write's moment is chosen by the test's events, not by
/// a sleep duration.
/// #1155 fix-r2b review (Codex P1-1): the cap's wall clock is re-checked at
/// the saw write itself, so a `started` that only appears after the cap —
/// even when a paused poll resumes to find the file already there — is not
/// recorded as seen.
/// The helper runs as its own process-group leader (`process_group(0)`), so
/// the stranger — and everything it runs — stays in a group the test alone
/// can end, and whose id is the spawn-returned one the `spawned` hook hands
/// over: the group exists from the spawn on, so it never depends on the
/// helper (or the stranger) publishing anything (#1141 fix-r2 review P1-1).
/// The stranger itself is the [`STRANGER_PROBE`] python process: it waits
/// for the release signal before publishing `started` (#1297 — so the
/// test, not a fixed sleep, moves the handshake), then for the go signal
/// the test publishes once the publication has returned (both capped, so
/// even a test that never signals leaves it bounded), observes the pipe,
/// records the verdict, and then parks in a bounded sleep that only the
/// test's [`StrangerGuard`] may end, never EOF or a self-chosen lifetime
/// (#1141 fix-r1 review P1-1).
fn dead_helper_with_a_stranger_reader(
    marker: &Path,
    go: &Path,
    started: &Path,
    saw: &Path,
    paused: &Path,
    resume: &Path,
    delay: Duration,
) -> process::Command {
    started_wait_helper(
        marker,
        go,
        started,
        saw,
        paused,
        resume,
        delay,
        stranger_started_wait_secs(),
    )
}

/// The helper shape of [`dead_helper_with_a_stranger_reader`] with the
/// started-wait parameters factored out (#1155 fix-r2b review Codex P1-1):
/// `wait_secs` is the wall-clock cap — derived from the one test-side
/// constant — and the pause is an explicit event barrier (#1155 fix-r3
/// review P1-1): the helper computes its `end` deadline, launches the
/// stranger, publishes the `paused` receipt (`$5`), and parks until the
/// test's `resume` signal (`$6`) appears. The park is bounded by its own
/// 30-second safety — deliberately *not* by `end`, because the whole point
/// of the barrier is that the saw write can be made to run after the
/// deadline, which a deadline-bounded pause would pre-empt by exiting the
/// wait early; the tests resume the helper long before the safety runs out,
/// and every guard or `ReapGuard` that owns the helper kills it on the way
/// out of a failing or panicking test. The `end` the cap resolves to is
/// computed before the stranger is launched, so both sides count from
/// before its start, and the saw write re-validates that same deadline: a
/// `started` that appeared after it is not recorded as seen just because
/// the file already exists when the paused poll resumes.
/// #1392 (#1155 r4 review P1, P2b follow-up): the park's own 30-second
/// safety timeout is a failure, never a silent pass. The forced re-check
/// after the loop — not the loop's own condition, which a `resume` landing
/// between its last time check and the exit could otherwise miss — exits 3
/// when no `resume` arrived, so a test thread and a stranger both starved
/// past the park cannot let the helper slip out with `exit 0` and a
/// skipped saw write while the test's `exited.success()` assertion passes
/// anyway. The saw write runs first, unchanged: the timeout path still
/// exercises the deadline re-check — a `started` that exists at the
/// timeout is past `end` and must not be recorded — which keeps a mutant
/// dropping that re-check killable on this path too.
#[allow(clippy::too_many_arguments)]
fn started_wait_helper(
    marker: &Path,
    go: &Path,
    started: &Path,
    saw: &Path,
    paused: &Path,
    resume: &Path,
    delay: Duration,
    wait_secs: u64,
) -> process::Command {
    let delay_ms = delay.as_millis();
    let mut helper = process::Command::new("/bin/sh");
    helper
        .arg("-c")
        .arg(format!(
            "exec 3<&0
               end=$(( $(date +%s) + {wait_secs} ))
               python3 -c '{STRANGER_PROBE}' \"$1\" \"$2\" \"$3\" {delay_ms} <&3 &
               printf paused >\"$5.staging\"; mv \"$5.staging\" \"$5\"
               pause_end=$(( $(date +%s) + 30 ))
               while [ ! -e \"$6\" ] && [ \"$(date +%s)\" -lt \"$pause_end\" ]; do /bin/sleep 0.05; done
               if [ -e \"$3\" ] && [ \"$(date +%s)\" -lt \"$end\" ]; then printf saw >\"$4.staging\"; mv \"$4.staging\" \"$4\"; fi
               if [ ! -e \"$6\" ]; then exit 3; fi
               exit 0"
        ))
        .arg("dead-helper")
        .arg(marker)
        .arg(go)
        .arg(started)
        .arg(saw)
        .arg(paused)
        .arg(resume)
        .stdin(process::Stdio::piped())
        .process_group(0);
    helper
}

/// The helper's bounded started-wait budget, in wall-clock seconds, derived
/// from the same [`STRANGER_STARTED_TIMEOUT`] the test-side wait uses
/// (#1155 r4 review P2): one cap, not two to drift apart — the r4 review
/// found the helper hardcoded at 5 s against the test's 10 s, so the
/// helper could give up on a stranger the test was still willing to wait
/// for. #1155 fix-r2b review (concurrency P2 / boundary P3): the `date +%s`
/// the helper counts on truncates to whole seconds, so a nominal cap can
/// lose up to one second of real budget; the `+ 1` compensates and keeps
/// the helper's true budget at or above the test-side cap. A helper that
/// out-waits the test is harmless — the test's own hook wait fails first.
fn stranger_started_wait_secs() -> u64 {
    STRANGER_STARTED_TIMEOUT.as_secs() + 1
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
        let mut watched = None;
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
                let stranger_pid = wait_for_stranger_pid(&pid_file);
                watched = Some(stranger_pid);
                stranger.follow(stranger_pid);
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
        // #1409 item 2: the pid-mode collection has to be *observed*. A
        // zombie still has a `/proc` entry, so the entry's disappearance is
        // what separates "reaped by this process" from "killed and left for
        // someone else" — the pid-mode counterpart of the group mode's
        // group-empty poll.
        assert_reaped_by_this_process(
            watched.expect("#1409 the spawned hook must record the stranger's pid"),
        );
        assert_only_the_owner_token_remains(&scratch);
    }

    /// `/bin/sh` leaves a background `sleep 30` holding the pipe's read end,
    /// then `exec`s its own sleep: killing the helper cannot close the read
    /// end, so only the writer's deadline ends the write. #1155 fix-r2b
    /// review (boundary P3): the stranger parks for the full 30 s — like the
    /// group-mode stranger's bounded sleep, only the guard's kill may end it
    /// — instead of self-exiting at 5 s, because a stranger that dies inside
    /// the guard's confirm window made the kill contract untestable: a guard
    /// that never killed still found it exited. The stranger's `<&3` off the
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
                "exec 3<&0; sleep 30 <&3 & \
                 echo $! >\"{staging}\"; mv \"{staging}\" \"{pid}\"; exec /bin/sleep 30",
                staging = staging.display(),
                pid = pid_file.display(),
            ))
            .arg("stalled-helper")
            .stdin(process::Stdio::piped());
        helper
    }

    /// #1409 item 2: the pid-mode collection's own evidence. An unreaped
    /// zombie still has a `/proc` entry, so the entry's disappearance is what
    /// separates "reaped by this process" from "killed and left for someone
    /// else to collect" — the pid-mode counterpart of the group mode's
    /// group-empty poll in [`StrangerGuard::reap_group`]. #2592 (#2142
    /// fix-r3 review, DS sibling #1): that evidence fails closed — only a
    /// vanished entry proves the collection ([`reap_evidence_is_gone`]), so
    /// the wait keeps polling on anything else and the deadline is what
    /// fails the assert, never a read error read as "already gone".
    fn assert_reaped_by_this_process(stranger: Pid) {
        let path = PathBuf::from(format!("/proc/{stranger}"));
        let deadline = Instant::now() + STRANGER_PID_REAP_TIMEOUT;
        while !reap_evidence_is_gone(&fs::metadata(&path)) && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(5));
        }
        assert!(
            reap_evidence_is_gone(&fs::metadata(&path)),
            "#1409 the pid-mode guard must reap the stranger: {} still exists or is unreadable, \
             so the stranger was only killed",
            path.display()
        );
    }

    /// #2592 (#2142 fix-r3 review, DS sibling #1): what a `/proc/<pid>`
    /// metadata read says about the pid-mode reap evidence. Only a vanished
    /// entry ([`metadata_failure_is_gone`]) is a disappearance — the one
    /// observation that separates "reaped by this process" from "killed and
    /// left for someone else to collect". Anything else keeps the stranger
    /// counted as un-reaped: a readable entry is still there to be
    /// collected, and a restricted procfs (hidepid, a restricted container,
    /// gVisor — the #1409 threat model this fixture's README itself adopts)
    /// that answers EACCES for the still-unreaped stranger leaves the
    /// entry's state unknown, which this evidence must read as present —
    /// `Path::exists()` turned every such read error into "already gone",
    /// the fail-open shape #2142 fix-r3 took out of the group mode's
    /// [`group_is_gone_within`], which could have false-PASSED the
    /// reap-removed mutant. Pinned by its own unit test the way the group
    /// rule is: a healthy host's `/proc` never answers the error side for
    /// a descendant.
    fn reap_evidence_is_gone(entry: &io::Result<fs::Metadata>) -> bool {
        match entry {
            Ok(_) => false,
            Err(error) => metadata_failure_is_gone(error),
        }
    }

    /// #2592: the error side of the pid-mode reap evidence, pinned the way
    /// `a_proc_read_error_means_gone_only_when_the_entry_is_absent` pins the
    /// group rule — a healthy host's `/proc` never answers the error side
    /// for a descendant, so the classification is the testable seam. Only a
    /// vanished entry means reaped: a procfs that hides the stranger behind
    /// EACCES must leave it counted as present, or the reap assert could
    /// pass a stranger that was only killed and never collected — the
    /// fail-open that could have false-PASSED the reap-removed mutant.
    #[test]
    fn a_reap_read_error_means_gone_only_when_the_entry_is_absent() {
        assert!(
            !reap_evidence_is_gone(&fs::metadata("/proc/self")),
            "#2592 a live /proc entry is not reap evidence"
        );
        assert!(
            reap_evidence_is_gone(&Err(io::Error::from(io::ErrorKind::NotFound))),
            "#2592 a vanished /proc entry is the one read error that means the stranger was \
             reaped"
        );
        assert!(
            !reap_evidence_is_gone(&Err(io::Error::from_raw_os_error(13))),
            "#2592 an unreadable /proc entry means the stranger's state is unknown, never reaped"
        );
        assert!(
            !reap_evidence_is_gone(&Err(io::Error::from_raw_os_error(5))),
            "#2592 an I/O error reading /proc means the stranger's state is unknown, never reaped"
        );
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
/// through to this — an explicit no-op, not a silent pass. #1155 fix-r2b
/// review (simplify P3): a stranger without a group would be an invariant
/// violation here — the only `watch`/`follow` callers off Linux own groups —
/// so the stub says so instead of passing silently.
#[cfg(not(target_os = "linux"))]
mod pid_mode {
    pub(super) fn end_stranger(guard: &mut super::StrangerGuard) -> std::io::Result<()> {
        debug_assert!(
            guard.stranger.is_none(),
            "#1155 an off-Linux guard owns a group; a pid-mode stranger cannot exist here"
        );
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

/// #1409 r3 review (concurrency P1): the zombie-leader probe parks a
/// descendant in exactly the state the return-boundary scan must fail closed
/// on — a zombie leader, a live thread group, and `/proc/<tgid>/fd` answering
/// EACCES for the whole settle window. A scan running on another libtest
/// thread inside that window classifies the probe's child as `Related` and
/// fails, which would turn an unrelated healthy test red. The probe and every
/// scan take this lock, so the poison state and a scan never overlap.
///
/// The probe is the only long holder — its own boot and pacing plus one
/// settle window, a few hundred milliseconds — so the cost to other scans is
/// one wait of that length, inside the `RETURN_GRACE` the publication callers
/// allow. Like the subreaper lease above, the lock is deliberately not a
/// mutation target: a mutant that drops it is only observable as a rare
/// interleaving, not as a deterministic failure.
#[cfg(target_os = "linux")]
static SCAN_WINDOW: Mutex<()> = Mutex::new(());

/// #2154 (probe OOM window): a guard that kills a whole probe process group.
/// The zombie-leader probes are the only tests that deliberately park a
/// descendant in the poison state — a zombie leader with a live thread group
/// and an EACCES fd directory — and the probe's `os.fork()`-to-`print(pid)`
/// span is exactly where an OOM-killed probe used to strand it: the test
/// panicked on the empty pid line, nothing killed the group, and the poison
/// outlived the released [`SCAN_WINDOW`] for as long as the surviving thread
/// slept. Both probes now run in their own process group (`process_group(0)`)
/// and take this guard before reading anything from them, so every exit path
/// — a passing run, an expect that fails, or a panic unwinding through the
/// test — kills the group. Rust drops locals in reverse declaration order,
/// and the guard is declared after the window lock, so on the unwind path the
/// group kill lands *before* the window is released. That is the whole
/// guarantee on the unwind path — the kill is issued, not confirmed: SIGKILL
/// is asynchronous, and a panicking thread must not sit waiting for a group
/// to die (#2142 fix-r1, concurrency r1 #4, correcting this comment's
/// earlier "never meets" claim). The passing path closes the gap the unwind
/// cannot, and the two probes close it differently (#2142 fix-r2, sibling r2
/// #2, correcting fix-r1's "both probes wait for the group's death"): the
/// EOF probe waits for the whole group's death before it drops the window,
/// so what a concurrent scan may meet there is only a group still dying
/// inside its own settle window; the leader probe waits only for the *hero's*
/// thread group — the poison state — and then drops this guard while the
/// probe leader (the python parent, single-threaded, parked in its own
/// `os.waitpid`) is still alive, so the kill lands on a running process, not
/// a corpse: what a concurrent scan may meet there is a process still
/// exiting, never the poison state — the same residue the scan's fail-closed
/// verdicts already absorb. The healthy path
/// keeps killing the probe's child by its own pid first (`KillOnDrop` in the
/// leader test); this guard is the backstop, not the primary kill.
#[cfg(target_os = "linux")]
struct KillProbeGroupOnDrop(Pid);

#[cfg(target_os = "linux")]
impl Drop for KillProbeGroupOnDrop {
    fn drop(&mut self) {
        let _ = rustix::process::kill_process_group(self.0, Signal::KILL);
    }
}

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
/// boundary (#1155 fix-r2b review, sibling P3): the mirror is deliberate —
/// a `#[path]` include cannot work, the lib module reads crate-private
/// siblings — so change this copy only together with the lib's.
fn group_exists(group: Pid) -> bool {
    rustix::process::test_kill_process_group(group).is_ok()
}

/// #1155 fix-r2b review (boundary P2 / concurrency P3): classify an fdinfo
/// read that failed behind an fd the scan had already resolved to the pipe.
/// The read can fail for a reason other than the fd's own disappearance — a
/// task in teardown, or one that exec'd a setuid binary, fails the procfs
/// access check with ESRCH/EACCES (cb8 probe, 6.17: an unreaped zombie's
/// fdinfo reads ENOENT while its fd link already reads EACCES, and a
/// setuid-exec'd child's fdinfo reads EACCES) — so the release is proven by
/// the fd itself, re-read here, and the re-read follows the same rule as
/// every other link read in this file (#1409): `ENOENT` is a release, an
/// unreadable link is one only when its holder is gone or already dead,
/// and an fd that still resolves to the pipe keeps the scan fail-closed.
#[cfg(target_os = "linux")]
fn fdinfo_unreadable(
    error: io::Error,
    fd: &Path,
    wanted: &str,
    sources: &mut BlindSources,
) -> Option<io::Error> {
    match fs::read_link(fd) {
        Ok(target) if target.to_string_lossy() == wanted => Some(io::Error::other(format!(
            "#1155 reading the fdinfo of {}: {error}",
            fd.display()
        ))),
        Ok(_) => None,
        // #1409: only a link that is gone proves the release — the same rule
        // the enumeration reads by ([`link_failure_is_a_release`]).
        Err(link) if link_failure_is_a_release(fd, &link, sources) => None,
        Err(link) => Some(io::Error::other(format!(
            "#1155 reading the fdinfo of {}: {error}; re-reading the fd itself: {link}",
            fd.display()
        ))),
    }
}

/// #1409: whether a `/proc` entry name is a pid — the same test the scan uses
/// on the directory listing, kept in one place so the two cannot diverge.
#[cfg(target_os = "linux")]
fn is_pid_name(name: &str) -> bool {
    !name.is_empty() && name.bytes().all(|byte| byte.is_ascii_digit())
}

/// #1409: the process behind a `/proc/<pid>/fd/<n>` path, or `None` when the
/// path does not name a numeric pid (`/proc/self/…`, a synthetic test path) —
/// an unknown holder, which the callers treat as still holding.
#[cfg(target_os = "linux")]
fn holder_of(fd: &Path) -> Option<&str> {
    let rest = fd.to_str()?.strip_prefix("/proc/")?;
    let pid = rest.split('/').next()?;
    is_pid_name(pid).then_some(pid)
}

/// #1409: whether a failed `read_link` on a descriptor path proves the fd is
/// released. Only `ENOENT` does; every other errno (EACCES after a setuid
/// exec or behind an LSM, EIO) leaves a descriptor the scan cannot see, and
/// the source decides: a holder that is gone or already dead holds nothing,
/// while this process, a descendant, or a holder whose state cannot be read
/// keeps the descriptor live as far as the scan is concerned. All three
/// `read_link` sites in this file — the enumeration, the fdinfo re-check and
/// the grace re-check — go through this one rule so they cannot drift apart.
#[cfg(target_os = "linux")]
fn link_failure_is_a_release(fd: &Path, error: &io::Error, sources: &mut BlindSources) -> bool {
    if error.kind() == io::ErrorKind::NotFound {
        return true;
    }
    match holder_of(fd) {
        Some(pid) => sources.of(pid).holds_nothing(),
        None => false,
    }
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
/// it is declared a survivor (the re-check itself is
/// [`retain_through_grace`], #1409 item 3). #1409: a directory that cannot
/// be read is classified *before* it is skipped ([`blind_source`]) — gone,
/// already dead, or unrelated to this process, none of which can hold a
/// copy of this pipe, versus this process or one of its descendants, whose
/// unreadable descriptors are an unknown the scan fails closed on. And
/// before any of that, [`a_write_end_this_process_holds_is_visible`] proves
/// the scan can see a write end this process holds through the very same
/// primitives, so a `/proc` that hides pipes makes the scan fail instead
/// of passing on nothing. An fd of the pipe whose fdinfo cannot
/// be read, or whose flags line cannot be parsed, is an unknown rather
/// than a read end: the release is proven by the fd itself, re-read after the
/// failed fdinfo read, and the re-read follows [`link_failure_is_a_release`]
/// (#1409) — `ENOENT`, or an unreadable link whose holder is gone or whose
/// thread group is dead; every other errno leaves a descriptor the scan
/// cannot see, and an fd that still resolves to the pipe fails the scan
/// instead of guessing the fd harmless (#1155 fix-r1 review, opus P2; the
/// pre-#1409 rule that any errno on the re-read proved the release is gone).
/// The pipe is identified by its inode —
/// both ends share it and no live pipe reuses it — and a write end by its
/// fdinfo access mode (`O_WRONLY`/`O_RDWR`, the low two flag bits); a read
/// end (mode `0`) is the stranger, and passes. `/proc` is Linux: elsewhere
/// the proof is skipped and the stranger's marker observation remains the
/// evidence.
fn assert_no_write_end_survives_the_return(pipe: u64) -> io::Result<()> {
    #[cfg(target_os = "linux")]
    {
        // #1409 r3 review (concurrency P1): a test that parks a live
        // descendant with unreadable descriptors holds this lock; see
        // [`SCAN_WINDOW`]. A poisoned mutex only means that test panicked
        // mid-probe, which must not cascade into unrelated scans.
        let _scan_window = SCAN_WINDOW
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        // #1409: the positive self-check comes first. A scan that cannot see
        // a write end this process holds itself — on hidepid, gVisor, or a
        // container that hides pipes from /proc — must fail, never pass on
        // an empty result that proves nothing.
        a_write_end_this_process_holds_is_visible()?;
        let wanted = format!("pipe:[{pipe}]");
        let mut candidates: Vec<String> = Vec::new();
        // #1409: the sources are counted rather than silently skipped, and
        // the scan refuses to report a clean result when it could not read a
        // single process: "saw nothing" is not evidence of "nothing there".
        let mut readable = 0usize;
        let mut skipped = 0usize;
        // #1409: one classification per holder per scan — see [`BlindSources`].
        let mut sources = BlindSources::default();
        let processes = fs::read_dir("/proc").map_err(|error| {
            io::Error::other(format!(
                "#1155 reading /proc for the return-boundary scan: {error}"
            ))
        })?;
        for process in processes {
            let process = process
                .map_err(|error| io::Error::other(format!("#1155 reading /proc: {error}")))?;
            let pid = process.file_name().to_string_lossy().into_owned();
            if !is_pid_name(&pid) {
                continue;
            }
            match fds_of(&pid, &wanted, &mut sources) {
                Ok(held) => {
                    readable += 1;
                    for (fd, access) in held {
                        if access & 0o3 == 0 {
                            // A read end — the stranger holds exactly that.
                            continue;
                        }
                        candidates.push(fd);
                    }
                }
                // #1409: a failed read is never silently skipped. A source
                // that cannot hold this pipe — gone, already dead, or not a
                // descendant of this process — may be passed over; anything
                // else is an unknown the scan fails closed on, because a
                // hidden descriptor of a descendant is exactly where a
                // leaked write end would be. The skip set lives in
                // [`BlindSource::holds_nothing`], the same predicate the link
                // rule ends in, so the two cannot drift apart.
                Err(error) => {
                    if sources.of(&pid).holds_nothing() {
                        skipped += 1;
                    } else {
                        return Err(error);
                    }
                }
            }
        }
        if readable == 0 {
            return Err(io::Error::other(format!(
                "#1409 the return-boundary scan could not read the descriptors of any process \
                 ({skipped} skipped): a clean result would prove nothing"
            )));
        }
        // A candidate counts as a surviving write end only if it persists
        // through the transient grace: the writer thread is finished, so
        // nothing new can acquire the descriptor, while a pre-exec fork of
        // this process loses its inherited copy at the exec already
        // underway.
        retain_through_grace(
            &mut candidates,
            &wanted,
            TRANSIENT_WRITE_END_GRACE,
            &mut sources,
        );
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

/// #1409: whether the process behind a failed `/proc` read could hold a
/// descriptor of this pipe at all. A descriptor of this pipe reaches another
/// process only by inheritance — down this process's descendants — so a
/// process that is gone, already dead, or unrelated can hold nothing the scan
/// must find. Everything else is an unknown: on a host that hides `/proc`,
/// treating an unreadable related process as "nothing there" is exactly how
/// the scan would report a leaked write end as clean.
#[cfg(target_os = "linux")]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum BlindSource {
    /// Gone between the `/proc` listing and the read: it holds nothing.
    Vanished,
    /// A zombie or a dead task: the kernel already closed its descriptors.
    Terminal,
    /// Not this process and not one of its descendants.
    Unrelated,
    /// This process, a descendant, or a task whose relation cannot be read.
    Related,
}

// #2248 (in passing): this impl alone missed the cfg the enum and every
// BlindSource helper carry (ed8ef1218); ungated it broke macOS clippy for the
// whole --all-targets gate — same rule as the 11-01 standing constraint.
#[cfg(target_os = "linux")]
impl BlindSource {
    /// #1409: whether a source can hold a descriptor of the scanned pipe at
    /// all. This is the one place the skip set is written — the enumeration
    /// and [`link_failure_is_a_release`] both end here, so a new variant or a
    /// narrowed set cannot silently apply on one side only.
    fn holds_nothing(self) -> bool {
        matches!(self, Self::Vanished | Self::Terminal | Self::Unrelated)
    }
}

/// #1409 r2 review (concurrency P2 / efficiency P3): one classification per
/// holder per scan. [`blind_source`] may spend the whole settle window on a
/// single holder, and a scan meets the same holder once per descriptor and
/// once per grace round — without this, one hidden holder would cost
/// N × `BLIND_SOURCE_SETTLE` inside a scan whose duration the callers bound,
/// and the enumeration would classify the same pid twice (once per failed
/// descriptor, once when `fds_of` reports the source).
///
/// #2154: what the memo freezes is the verdict of that pid's *first* settle
/// window — a snapshot at settle-timeout, not a live classification, and the
/// cache is safe on exactly one side. The skip verdicts are final: under the
/// assumptions of one scan — no new holder of the pipe can appear, the same
/// pid-reuse caveat [`descended_from_this_process`] already carries — a
/// `Terminal`/`Vanished` source cannot become live again and an `Unrelated`
/// one cannot become a descendant. A cached `Related` is the opposite side:
/// it can only be *stricter* than a fresh verdict, never looser — the source
/// it froze may since have settled into `Terminal`, and the memo keeps
/// failing closed on it instead of skipping it. Re-settling a cached
/// `Related` per lookup would spend up to another `BLIND_SOURCE_SETTLE`
/// each time (the very N × settle this memo bounds) only to turn some
/// fail-closed verdicts into skips — the one direction this cache must
/// never move.
///
/// It is a per-scan, single-threaded cache (r3 review, concurrency P2): one
/// instance lives on the scan's stack and is never shared or reused, so its
/// unbounded key set is bounded by the number of pids in `/proc` at that
/// moment. A first lookup for a holder that needs the settle blocks for up to
/// `BLIND_SOURCE_SETTLE`; later lookups of the same pid are a hash probe.
#[cfg(target_os = "linux")]
#[derive(Default)]
struct BlindSources {
    seen: HashMap<String, BlindSource>,
}

#[cfg(target_os = "linux")]
impl BlindSources {
    /// The classification of `pid`, decided once per scan: the verdict its
    /// first lookup settled on, frozen for the rest of this scan.
    fn of(&mut self, pid: &str) -> BlindSource {
        *self
            .seen
            .entry(pid.to_owned())
            .or_insert_with(|| blind_source(pid))
    }
}

/// #1409: the state and parent fields of `/proc/<pid>/stat`. Everything is
/// read after the *last* `)` because the comm field between the parentheses
/// may contain spaces and parentheses of its own.
#[cfg(target_os = "linux")]
fn stat_fields(stat: &str) -> Option<(char, &str)> {
    let (_, tail) = stat.rsplit_once(')')?;
    let mut fields = tail.split_whitespace();
    let state = fields.next()?.chars().next()?;
    let parent = fields.next()?;
    Some((state, parent))
}

/// #1409: whether `pid` is this process or one of its descendants — the only
/// processes that can hold a copy of a descriptor this process owns. An
/// unreadable chain is an unknown and counts as related, so it fails the scan
/// closed rather than passing it. Known limits, both needing pid reuse to be
/// reached at all: a `SCM_RIGHTS` handoff is inheritance the parent chain does
/// not show, and an ancestor reaped and its pid reused mid-walk would send the
/// chain into an unrelated tree (the failure there is a skipped source, and
/// only for sources already unreadable).
#[cfg(target_os = "linux")]
fn descended_from_this_process(pid: &str) -> bool {
    let mine = process::id().to_string();
    let mut current = pid.to_owned();
    for _ in 0..64 {
        if current == mine {
            return true;
        }
        let Ok(stat) = fs::read_to_string(format!("/proc/{current}/stat")) else {
            return true;
        };
        let Some((_, parent)) = stat_fields(&stat) else {
            return true;
        };
        if parent == "0" {
            return false;
        }
        current = parent.to_owned();
    }
    true
}

/// #1409: whether a `Z`/`X` state means the whole *thread group* is dead, and
/// with it the group's descriptor table. A thread-group leader can be a
/// zombie while its group lives on (`pthread_exit` in the leader): the
/// surviving threads share the fd table, so a descriptor is still open, and
/// `/proc/<tgid>/fd` answers EACCES in exactly that state — the very read the
/// scan classifies (cb8 6.17, `_tmp/20261107-issue-1409/
/// zombie-leader-probe3.log`: leader `Z`, worker `S`, fd directory EACCES,
/// and a non-blocking read of the pipe's read end EAGAIN — a write end is
/// still open). So a zombie leader alone proves nothing; only a group whose
/// every task is `Z`/`X` has closed its descriptors. Anything unreadable is
/// *not* dead, so the caller keeps treating the source as live.
#[cfg(target_os = "linux")]
fn thread_group_is_dead(pid: &str) -> bool {
    let Ok(tasks) = fs::read_dir(format!("/proc/{pid}/task")) else {
        return false;
    };
    let mut seen = 0usize;
    for task in tasks {
        let Ok(task) = task else { return false };
        let Ok(stat) = fs::read_to_string(task.path().join("stat")) else {
            return false;
        };
        if !matches!(stat_fields(&stat), Some(('Z' | 'X', _))) {
            return false;
        }
        seen += 1;
    }
    seen != 0
}

/// #2142 (#1409 r3 simplify review): the one definition of "this state means
/// the source is terminal" — `Z`/`X` alone proves nothing (a zombie leader
/// with a live thread group still shares its fd table, see
/// [`thread_group_is_dead`]), only a dead thread group behind the state does.
/// [`blind_source`] and [`settles_into_a_terminal_source`] both end here, so
/// the enumeration's first check and the settle re-check cannot disagree on
/// the same pid — the two used to write the criterion out twice, and the
/// negative controls anchored only the enumeration's copy. #2142 fix-r1
/// (boundary r1 #4): the production sweep (`src/test_process.rs`, #2138) has
/// its own `terminal_state` — same shape, separate crate boundary — so this is
/// the one definition *here*, and the two copies change together or not at
/// all. #2142 fix-r2 (sibling r2 #2): "together" is machine-checked on both
/// sides now — each copy's body is a negative-control anchor
/// (`terminal-state-hardcoded-false` here,
/// `terminal-state-production-hardcoded-false` there), and each crate's unit
/// test pins the Terminal verdict of both entries on one dead pid.
#[cfg(target_os = "linux")]
fn terminal_state(pid: &str, state: char) -> bool {
    matches!(state, 'Z' | 'X') && thread_group_is_dead(pid)
}

/// #1409: classify a source whose `/proc` reads failed, for the scan's
/// "may this be skipped" decision.
#[cfg(target_os = "linux")]
fn blind_source(pid: &str) -> BlindSource {
    let stat = match fs::read_to_string(format!("/proc/{pid}/stat")) {
        Ok(stat) => stat,
        // No stat at all: the task is gone between the listing and the read.
        Err(error) if error.kind() == io::ErrorKind::NotFound => return BlindSource::Vanished,
        // Any other failure leaves the relation unknown: never skipped.
        Err(_) => return BlindSource::Related,
    };
    let Some((state, _)) = stat_fields(&stat) else {
        return BlindSource::Related;
    };
    // A zombie leader with a live group has not released its descriptors, so
    // "terminal" is the whole group being dead, not one task reporting `Z`
    // (#2142: one criterion, shared with the settle re-check).
    if terminal_state(pid, state) {
        return BlindSource::Terminal;
    }
    if descended_from_this_process(pid) {
        settles_into_a_terminal_source(pid)
    } else {
        BlindSource::Unrelated
    }
}

/// #1409: how long a related source may still be finishing its exit before
/// its unreadable descriptors count as hidden while alive. `exit_files()`
/// runs before `exit_notify()` marks the task a zombie, and in that window
/// the fd directory already fails (cb8 6.17 answers EACCES — measured, see
/// `_tmp/20261107-issue-1409/exit-window-probe-3000.log`) while
/// `/proc/<pid>/stat` still says `R`. Failing on that first read would turn a
/// healthy run red; failing only after the source settles keeps a live
/// descendant with hidden descriptors a hard failure.
#[cfg(target_os = "linux")]
const BLIND_SOURCE_SETTLE: Duration = Duration::from_millis(50);

/// #1409: re-read a related source's state until it either proves terminal —
/// a dead thread group, a dead task, or gone — or the settle window runs out,
/// in which case it is still an unknown and the scan fails closed on it. A
/// zombie leader alone is not terminal: the verdict is [`terminal_state`]'s,
/// the one definition the enumeration's first check also uses (#2142).
#[cfg(target_os = "linux")]
fn settles_into_a_terminal_source(pid: &str) -> BlindSource {
    let deadline = Instant::now() + BLIND_SOURCE_SETTLE;
    loop {
        match fs::read_to_string(format!("/proc/{pid}/stat")) {
            Ok(stat) => match stat_fields(&stat) {
                Some((state, _)) if terminal_state(pid, state) => {
                    return BlindSource::Terminal;
                }
                Some(_) => {}
                None => return BlindSource::Related,
            },
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return BlindSource::Vanished;
            }
            Err(_) => {}
        }
        if Instant::now() >= deadline {
            return BlindSource::Related;
        }
        thread::sleep(Duration::from_millis(2));
    }
}

/// #1409: the descriptors of `pid` that resolve to `wanted`, with their
/// fdinfo access mode. Every failed read is reported — the caller decides
/// whether its *source* can be passed over — except a link whose failure
/// proves the fd released ([`link_failure_is_a_release`]): `ENOENT`, or any
/// other errno behind a holder that is gone or whose thread group is dead.
#[cfg(target_os = "linux")]
fn fds_of(pid: &str, wanted: &str, sources: &mut BlindSources) -> io::Result<Vec<(String, u32)>> {
    let mut held = Vec::new();
    let descriptors = fs::read_dir(format!("/proc/{pid}/fd")).map_err(|error| {
        io::Error::other(format!("#1409 reading the descriptors of {pid}: {error}"))
    })?;
    for descriptor in descriptors {
        let descriptor = descriptor.map_err(|error| {
            io::Error::other(format!(
                "#1409 enumerating the descriptors of {pid}: {error}"
            ))
        })?;
        let target = match fs::read_link(descriptor.path()) {
            Ok(target) => target,
            // #1409: only a link that vanished between the enumeration and
            // the read is a release. Any other errno (EACCES from a setuid
            // exec or an LSM, EIO) leaves a descriptor the scan cannot see
            // — the source decides, and a related one fails the scan closed.
            Err(error) if link_failure_is_a_release(&descriptor.path(), &error, sources) => {
                continue;
            }
            Err(error) => {
                return Err(io::Error::other(format!(
                    "#1409 reading the descriptor {} of {pid}: {error}",
                    descriptor.path().display()
                )));
            }
        };
        if target.to_string_lossy() != wanted {
            continue;
        }
        let number = descriptor.file_name().to_string_lossy().into_owned();
        let fdinfo = match fs::read_to_string(format!("/proc/{pid}/fdinfo/{}", number)) {
            Ok(fdinfo) => fdinfo,
            // #1155 fix-r2b review (boundary P2 / concurrency P3): the
            // release is proven by the fd itself, re-read here under the
            // same [`link_failure_is_a_release`] rule as every other link
            // read in this file — see [`fdinfo_unreadable`].
            Err(error) => {
                if let Some(failure) = fdinfo_unreadable(error, &descriptor.path(), wanted, sources)
                {
                    return Err(failure);
                }
                continue;
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
                    "#1155 the fdinfo of /proc/{pid}/fd/{number} has no parsable flags line"
                )));
            }
        };
        held.push((format!("/proc/{pid}/fd/{number}"), access));
    }
    Ok(held)
}

/// #1409: the scan's own eyes. Before a clean scan result is trusted, the
/// scan must see a write end this process provably holds through the very
/// same primitives it uses on everyone else — `/proc/<pid>/fd`, `read_link`,
/// and the fdinfo access mode. A host that hides pipes or the descriptors of
/// processes from `/proc` (hidepid, gVisor, a restricted container) then
/// makes the scan fail instead of passing on nothing. The canary is a fresh
/// pipe of this process's own: the publication pipe may legitimately have no
/// fd left at the return boundary (the helper is reaped and the writer
/// thread dropped its end), so the self-check must not depend on it.
#[cfg(target_os = "linux")]
fn a_write_end_this_process_holds_is_visible() -> io::Result<()> {
    let (_reader, writer) = io::pipe().map_err(|error| {
        io::Error::other(format!(
            "#1409 creating the return-boundary scan's self-check pipe: {error}"
        ))
    })?;
    let inode = rustix::fs::fstat(writer.as_fd())
        .map_err(|error| io::Error::other(format!("#1409 fstat of the self-check pipe: {error}")))?
        .st_ino;
    let wanted = format!("pipe:[{inode}]");
    let mine = process::id().to_string();
    if fds_of(&mine, &wanted, &mut BlindSources::default())?
        .iter()
        .any(|(_, access)| access & 0o3 != 0)
    {
        return Ok(());
    }
    Err(io::Error::other(format!(
        "#1409 the return-boundary scan cannot see the write end of a pipe it holds itself \
         ({wanted}): this host's /proc hides it, so a clean scan would prove nothing"
    )))
}

/// #1409 item 3: the transient-write-end grace. A candidate counts as a
/// surviving write end only if it persists through the grace — a release
/// makes an fd disappear for good, while a pre-exec fork of this process
/// loses its inherited copy at the exec already underway — so the re-check
/// evicts every candidate whose fd no longer resolves to the pipe, and keeps
/// every one that does or that it cannot read behind a live holder (the same
/// [`link_failure_is_a_release`] rule the enumeration reads by), until the
/// grace runs out.
#[cfg(target_os = "linux")]
fn retain_through_grace(
    candidates: &mut Vec<String>,
    wanted: &str,
    grace: Duration,
    sources: &mut BlindSources,
) {
    let deadline = Instant::now() + grace;
    while !candidates.is_empty() && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(25));
        candidates.retain(|fd| match fs::read_link(fd) {
            Ok(target) => target.to_string_lossy() == wanted,
            // #1409: an unreadable link is not a release unless its holder
            // is gone or already dead — the same rule as the enumeration, so
            // a descriptor hidden mid-grace stays a survivor instead of
            // being quietly dropped from the verdict.
            Err(error) => !link_failure_is_a_release(Path::new(fd), &error, sources),
        });
    }
}

/// #1155 fix-r2b review (boundary P2 / concurrency P3): the two outcomes of
/// an fdinfo read that failed behind an already-matched fd — a live fd
/// fails the scan closed, a gone fd proves the release — pinned without the
/// kernel race that produces the errno in the wild (ESRCH stands in for it
/// here).
#[cfg(target_os = "linux")]
#[test]
fn an_unreadable_fdinfo_fails_closed_only_behind_a_live_fd() {
    use std::os::fd::AsRawFd;
    let esrch = || io::Error::from_raw_os_error(3);
    let held = fs::File::open("/dev/null").expect("#1155 open the fdinfo probe descriptor");
    let fd = format!("/proc/self/fd/{}", held.as_raw_fd());
    let wanted = fs::read_link(&fd)
        .expect("#1155 read the fdinfo probe descriptor's target")
        .to_string_lossy()
        .into_owned();
    let unknown = fdinfo_unreadable(
        esrch(),
        Path::new(&fd),
        &wanted,
        &mut BlindSources::default(),
    )
    .expect("#1155 an unreadable fdinfo behind a live fd must fail the scan closed");
    assert!(
        unknown.to_string().contains("#1155 reading the fdinfo"),
        "#1155 the fail-closed error must name the fdinfo read: {unknown}"
    );
    // A descriptor number nothing holds — released, in the scan's terms —
    // cannot be shown to exist, so its unreadable fdinfo is a release.
    let gone = Path::new("/proc/self/fd/1048576");
    assert!(
        fdinfo_unreadable(esrch(), gone, &wanted, &mut BlindSources::default()).is_none(),
        "#1155 an fd that is gone must prove the release of its unreadable fdinfo"
    );
}

/// #1409: the stat parse must survive a comm field with spaces and
/// parentheses — every field is read after the *last* `)`, because comm may
/// contain both.
#[cfg(target_os = "linux")]
#[test]
fn the_stat_fields_are_read_after_the_last_parenthesis() {
    let (state, parent) = stat_fields("42 (a ) b (c) Z 7 42 42 0 -1 4194304")
        .expect("#1409 the stat fields of a comm with parentheses must parse");
    assert_eq!(
        (state, parent),
        ('Z', "7"),
        "#1409 the state and parent follow the last ')'"
    );
    assert!(
        stat_fields("no parentheses here").is_none(),
        "#1409 a stat without its comm parentheses must not be guessed at"
    );
}

/// #1409: the classification that decides whether a failed `/proc` read may
/// be passed over. This process is always related — its own descriptors are
/// what the scan reads first — init cannot descend from this test process,
/// and a reaped child is gone. #2142: this test also walks the settle entry —
/// `blind_source` of this very process passes the enumeration's first check
/// as non-terminal and descends into [`settles_into_a_terminal_source`] — so
/// it is where the Terminal side of the shared [`terminal_state`] criterion
/// is pinned, through both entries on one dead pid: a killed, un-reaped group
/// with no live task must read `Terminal` from the enumeration's first check
/// and from the settle re-check alike. One assert holds both verdicts, so a
/// criterion that drifts — or is hardcoded away — shows both halves flipping
/// in the failure payload, not only the fail-open side the zombie-leader
/// mutants pin on their probe.
#[cfg(target_os = "linux")]
#[test]
fn an_unreadable_source_is_skipped_only_when_it_cannot_hold_the_pipe() {
    let mine = process::id().to_string();
    assert_eq!(
        blind_source(&mine),
        BlindSource::Related,
        "#1409 this process is the scan's own source and must never be skipped"
    );
    // Init cannot descend from this process — unless the test binary *is*
    // PID 1 (a container entrypoint), in which case the self check above is
    // the same assertion and this one must not contradict it; and on a host
    // that hides other users' processes (`hidepid`) `/proc/1/stat` is not
    // readable, which is the blind case this scan exists to fail on, not a
    // classification result to assert.
    if mine != "1" && Path::new("/proc/1/stat").exists() {
        assert_eq!(
            blind_source("1"),
            BlindSource::Unrelated,
            "#1409 init cannot descend from this process, so it can never hold this pipe"
        );
    }
    let mut child = process::Command::new("/bin/true")
        .spawn()
        .expect("#1409 spawn the vanished-source probe");
    let pid = child.id().to_string();
    let _ = child.wait().expect("#1409 reap the vanished-source probe");
    assert_eq!(
        blind_source(&pid),
        BlindSource::Vanished,
        "#1409 a reaped process is gone and holds nothing"
    );
    // #2142: the Terminal verdict of the shared criterion, through both
    // entries, on one pid. The probe is killed but not reaped, so it is still
    // in `/proc` as an all-`Z` group — terminal for the enumeration's first
    // check (inside `blind_source`) and for the settle re-check (called
    // directly, the entry a related-but-alive source reaches when the first
    // check does not fire). Both calls are one expression, so the mutant that
    // hardcodes the criterion away fails with both verdicts in its payload.
    let mut dead = process::Command::new("sleep")
        .arg("30")
        .spawn()
        .expect("#2142 spawn the dead-group probe");
    let dead_pid = dead.id().to_string();
    dead.kill().expect("#2142 kill the dead-group probe");
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let stat = fs::read_to_string(format!("/proc/{dead_pid}/stat")).unwrap_or_default();
        if matches!(stat_fields(&stat), Some(('Z', _))) {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "#2142 the dead-group probe must become a zombie, not stay {stat:?}"
        );
        thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(
        (
            blind_source(&dead_pid),
            settles_into_a_terminal_source(&dead_pid)
        ),
        (BlindSource::Terminal, BlindSource::Terminal),
        "#2142 a dead thread group must be terminal through both the enumeration and the settle"
    );
    let _ = dead.wait().expect("#2142 reap the dead-group probe");
}

/// #1409 r2 review (concurrency): a `Z` state is the leader's, not the
/// group's. A killed task with no live thread left reports `Z` for its only
/// task and has really closed its descriptors, while this running process has
/// not — the two directions `thread_group_is_dead` must tell apart.
#[cfg(target_os = "linux")]
#[test]
fn a_zombie_leader_alone_does_not_close_its_groups_descriptors() {
    let mine = process::id().to_string();
    assert!(
        !thread_group_is_dead(&mine),
        "#1409 this process is running, so its thread group is not dead"
    );
    let mut child = process::Command::new("sleep")
        .arg("30")
        .spawn()
        .expect("#1409 spawn the zombie probe");
    let pid = child.id().to_string();
    child.kill().expect("#1409 kill the zombie probe");
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let stat = fs::read_to_string(format!("/proc/{pid}/stat")).unwrap_or_default();
        if matches!(stat_fields(&stat), Some(('Z', _))) {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "#1409 the killed probe must become a zombie, not stay {stat:?}"
        );
        thread::sleep(Duration::from_millis(5));
    }
    assert!(
        thread_group_is_dead(&pid),
        "#1409 a zombie with no live task left has closed its descriptors"
    );
    let _ = child.wait();
}

/// #2142 fix-r2 (boundary r2 #1 / MM boundary r2 #1 / simplify r2 #2): the
/// survivor's hold cap — the one number every probe budget below is derived
/// against, injected into both probe scripts so the two cannot drift apart.
/// The margin itself is a compile-time fact, not prose: the pid line's own
/// budget plus the receipt, zombie and kill-window budgets must all fit
/// inside the hold cap, or the kill-removed mutant can be outwaited by the
/// front path's own clocks on a slow host — the failure fix-r1 structured
/// away, whose last unbounded wait (the pid-line read itself) this round
/// closes with `PROBE_LINE_BUDGET`.
#[cfg(target_os = "linux")]
const PROBE_HOLD_SECS: u64 = 30;

/// #2142 fix-r2: the front-path budgets of the two probe tests — every wait
/// fails red inside its own budget before it can silently overrun. The
/// leader test's chain is the shorter one (no receipt wait), so the margin
/// assert below covers both.
#[cfg(target_os = "linux")]
const PROBE_LINE_BUDGET: Duration = Duration::from_secs(4);

#[cfg(target_os = "linux")]
const PROBE_RECEIPT_BUDGET: Duration = Duration::from_secs(10);

#[cfg(target_os = "linux")]
const PROBE_ZOMBIE_BUDGET: Duration = Duration::from_secs(10);

#[cfg(target_os = "linux")]
const PROBE_KILL_WINDOW: Duration = Duration::from_secs(5);

/// How long a timeout branch waits for its own explicit kill to land before
/// it escalates — the kill's confirmation, so the window is released only
/// behind a group that is provably dying, the same order the healthy path
/// keeps.
#[cfg(target_os = "linux")]
const PROBE_DEATH_CONFIRM: Duration = Duration::from_secs(1);

/// #2142 fix-r3 (concurrency P1, two reviewers): the confirmation escalates
/// once — a first SIGKILL that has not landed within [`PROBE_DEATH_CONFIRM`]
/// gets a second kill and this named window before the branch gives up and
/// panics, so the panic never releases the window over a group that was only
/// presumed dying (the fix-r2 shape dropped the verdict on the floor with
/// `let _ =`, which is the gap this closes).
#[cfg(target_os = "linux")]
const PROBE_DEATH_RETRY_WINDOW: Duration = Duration::from_secs(3);

/// The kill margin as a machine-checked fact (#2142 fix-r2, boundary r2 #1 /
/// MM boundary r2 #1): the EOF test's whole front path — pid line + receipt +
/// zombie + kill window — must fit inside the survivor's hold cap, or the
/// kill-removed mutant stops being provably killable on a slow host. `Duration`
/// addition is not const, so the budgets are compared as seconds.
#[cfg(target_os = "linux")]
const _: () = assert!(
    PROBE_LINE_BUDGET.as_secs()
        + PROBE_RECEIPT_BUDGET.as_secs()
        + PROBE_ZOMBIE_BUDGET.as_secs()
        + PROBE_KILL_WINDOW.as_secs()
        < PROBE_HOLD_SECS
);

/// #2142 fix-r2 (boundary r2 #1 / MM boundary r2 #1): one line from a probe's
/// piped stdout, read on a worker thread with its own budget — `read_line`
/// has no deadline of its own, and leaving it unbounded in front of the
/// receipt/zombie/kill budgets would let a slow host eat the kill margin from
/// ahead (the hold thread's 30s clock starts at the fork, the budget sums
/// only at the read). The reader is detached on purpose: a budget overrun
/// panics while the read is still parked, and the pipe's write end is the
/// probe's own lifetime (parent exit plus the child's first-act close), so
/// the thread cannot outlive the probe it serves.
#[cfg(target_os = "linux")]
fn read_probe_line(stdout: process::ChildStdout, budget: Duration) -> (usize, String) {
    use std::io::BufRead;
    let (line_tx, line_rx) = mpsc::channel();
    thread::Builder::new()
        .name("#2142 probe pid line".into())
        .spawn(move || {
            let mut stdout = io::BufReader::new(stdout);
            let mut line = String::new();
            let _ = line_tx.send(stdout.read_line(&mut line).map(|read| (read, line)));
        })
        .expect("#2142 spawn the probe pid-line reader");
    match line_rx.recv_timeout(budget) {
        Ok(Ok(line)) => line,
        Ok(Err(error)) => panic!("#2142 reading the probe's pid line failed: {error}"),
        Err(_) => panic!(
            "#2142 the probe's pid line must arrive within its own {}s budget",
            budget.as_secs()
        ),
    }
}

/// #2142 fix-r2 (simplify r2 #3 / MM simplify r2 #4): the one wait both probe
/// tests share — the group behind `pid` is gone from `/proc` (already reaped)
/// or entirely `Z`/`X`, within `budget`, at a 5 ms cadence. Returns whether
/// it settled in time; the caller owns the failure action, so the
/// mutant-bound killpg line stays inside [`confirm_probe_group_death`], a
/// different line from the guard's Drop the `probe-group-kill-removed` mutant
/// removes. #2142 fix-r3 (concurrency #1): the wait fails closed — a `/proc`
/// read that is anything but "no entry" (see [`metadata_failure_is_gone`])
/// counts as alive, so a restricted procfs cannot hide a live group behind a
/// read error and still deliver a confirmed death, the fail-open shape that
/// could have false-PASSED the kill-removed mutant.
#[cfg(target_os = "linux")]
fn group_is_gone_within(pid: &str, budget: Duration) -> bool {
    let deadline = Instant::now() + budget;
    loop {
        let gone = match fs::metadata(format!("/proc/{pid}")) {
            // The usual exit: reaped, and gone from the tree.
            Err(error) if metadata_failure_is_gone(&error) => true,
            // Anything else unreadable is an unknown — alive for a wait
            // whose whole job is proving death.
            Err(_) => false,
            Ok(_) => thread_group_is_dead(pid),
        };
        if gone {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        thread::sleep(Duration::from_millis(5));
    }
}

/// #2142 fix-r3 (concurrency #1): what a failed `/proc/<pid>` metadata read
/// means for the group-is-gone wait — only a vanished entry is a
/// disappearance. Any other failure (a restricted procfs answers EACCES for
/// a live hero) leaves the group's state unknown, which the wait must count
/// as alive: the same fail-closed side [`blind_source`] keeps for unreadable
/// sources. Pinned by its own unit test — a healthy host's `/proc` never
/// answers the error side for a descendant, the same reason
/// [`an_unreadable_link_is_a_release_only_when_its_holder_is_gone`] pins the
/// link rule instead of forcing a real unreadable link.
#[cfg(target_os = "linux")]
fn metadata_failure_is_gone(error: &io::Error) -> bool {
    error.kind() == io::ErrorKind::NotFound
}

/// #2142 fix-r3 (concurrency P1, two reviewers): the timeout branch's kill
/// confirmation, made a real assertion. The fix-r2 shape dropped the verdict
/// (`let _ = group_is_gone_within(..)`), so a group that outlived its own
/// SIGKILL — a D-state task past the confirm budget — still saw the branch
/// panic release the window over it. Now a false first confirm escalates
/// once, a second kill and [`PROBE_DEATH_RETRY_WINDOW`], and only a group
/// that stays live through both fails the assert; a confirmed death returns
/// to the caller, whose own panic is the failure the branch exists for.
#[cfg(target_os = "linux")]
fn confirm_probe_group_death(group: Pid, hero: &str) {
    rustix::process::kill_process_group(group, Signal::KILL).ok();
    if group_is_gone_within(hero, PROBE_DEATH_CONFIRM) {
        return;
    }
    rustix::process::kill_process_group(group, Signal::KILL).ok();
    assert!(
        group_is_gone_within(hero, PROBE_DEATH_RETRY_WINDOW),
        "#2142 the timeout branch must not panic over a group whose death it could not confirm"
    );
}

/// #2142 fix-r2 (concurrency r2 #1 / boundary r2 #3): collect the probe's
/// orphaned hero when a concurrent stranger guard's child-subreaper lease has
/// parked it on this process — in the bounded shape of the file's other
/// waits (`EXITED | NOHANG` with its own deadline, never a blocking waitid:
/// a recycled pid must not make this test wait on, or consume the exit state
/// of, somebody else's child). ECHILD means init owns the hero and has
/// already collected it. Either way this is reaping, not asserting, so no
/// outcome fails the test.
#[cfg(target_os = "linux")]
fn reap_probe_orphan(hero: i32) {
    let pid = Pid::from_raw(hero).expect("#2142 the probe hero's pid must be valid");
    let deadline = Instant::now() + PROBE_DEATH_CONFIRM;
    while Instant::now() < deadline {
        match rustix::process::waitid(
            rustix::process::WaitId::Pid(pid),
            rustix::process::WaitIdOptions::EXITED | rustix::process::WaitIdOptions::NOHANG,
        ) {
            Ok(Some(_)) | Err(_) => return,
            Ok(None) => thread::sleep(Duration::from_millis(5)),
        }
    }
}

/// #1409 r2 review (concurrency): a zombie thread-group leader whose group
/// still has a live thread has *not* released the group's descriptors. The
/// kernel reports `Z` for the leader, answers EACCES on the group's fd
/// directory, and the surviving thread's write end is still open (cb8 6.17,
/// `_tmp/20261107-issue-1409/zombie-leader-probe3.log`), so classifying `Z`
/// as terminal would skip exactly the source that still holds the pipe — a
/// clean verdict over a leak. The probe is the established `python3` kind:
/// the leader leaves through a real `pthread_exit()` while a second thread
/// keeps the group alive.
///
/// r3 review (concurrency P1): the probe deliberately holds a descendant in
/// the state every scan must fail closed on, for about one settle window.
/// [`SCAN_WINDOW`] keeps a concurrent scan out of that window — otherwise this
/// test would make unrelated healthy tests flake. #2154: the probe runs in
/// its own process group with [`KillProbeGroupOnDrop`] taken before the
/// first read, and its child closes the inherited stdout copy as its first
/// act, so a probe that dies between its `os.fork()` and the pid line
/// delivers EOF at the death — not held off until the surviving thread
/// exits — and the panic on the empty pid line unwinds through the group
/// kill, before the window is released. #2142 fix-r1 (boundary r1 #1,
/// measured): without that close the read would block on the child's copy
/// for the survivor's full 30s while holding the scan window — far past the
/// `RETURN_GRACE` the publication callers allow — and the panic the guard
/// exists for would never happen (`_tmp/20261026-issue-2142/fix-r1/
/// oom-eof-probe-ab.log`).
#[cfg(target_os = "linux")]
#[test]
fn a_live_group_behind_a_zombie_leader_is_not_terminal() {
    let _scan_window = SCAN_WINDOW
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    // #2142 fix-r2 (simplify r2 #2): the survivor's hold cap is one constant
    // shared with the EOF probe below, injected here — two handwritten `30`s
    // could drift until one of them ate the kill margin the budgets protect.
    let probe_script = format!(
        r#"
import ctypes, os, threading, time
read_end, write_end = os.pipe()
pid = os.fork()
if pid == 0:
    os.close(1)
    def hold():
        os.write(write_end, b"x")
        time.sleep({hold})
    threading.Thread(target=hold, daemon=False).start()
    time.sleep(0.2)
    ctypes.CDLL("libc.so.6", use_errno=True).pthread_exit(None)
    os._exit(9)
os.close(write_end)
print(pid, flush=True)
os.waitpid(pid, 0)
"#,
        hold = PROBE_HOLD_SECS
    );
    // Kill the probe's child on the way out, so neither the panic path nor a
    // passing run leaves a stray sleeper behind.
    struct KillOnDrop(Pid);
    impl Drop for KillOnDrop {
        fn drop(&mut self) {
            let _ = rustix::process::kill_process(self.0, Signal::KILL);
        }
    }

    let mut probe = process::Command::new("python3")
        .arg("-I")
        .arg("-c")
        .arg(&probe_script)
        .stdout(process::Stdio::piped())
        .process_group(0)
        .spawn()
        .expect("#1409 spawn the zombie-leader probe");
    // #2154: the guard is taken before the first read of the probe — the span
    // between the probe's `os.fork()` and its `print(pid)` is where an
    // OOM-killed probe strands the poison state, and a panic on the empty pid
    // line unwinds through this drop (the group kill) before it releases the
    // scan window above.
    let group = Pid::from_raw(probe.id() as i32)
        .expect("#1409 the zombie-leader probe's pid must be valid");
    let _probe_group = KillProbeGroupOnDrop(group);
    let (_read, pid_line) = read_probe_line(
        probe
            .stdout
            .take()
            .expect("#1409 take the zombie-leader probe's stdout"),
        PROBE_LINE_BUDGET,
    );
    let hero_raw: i32 = pid_line
        .trim()
        .parse()
        .expect("#1409 the zombie-leader probe must print a numeric child pid");
    let hero = hero_raw.to_string();
    let kill_on_drop = KillOnDrop(
        Pid::from_raw(hero_raw).expect("#1409 the probe's child pid must be a valid pid"),
    );
    let deadline = Instant::now() + PROBE_ZOMBIE_BUDGET;
    loop {
        let stat = fs::read_to_string(format!("/proc/{hero}/stat")).unwrap_or_default();
        if matches!(stat_fields(&stat), Some(('Z', _))) {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "#1409 the probe's leader must become a zombie, not stay {stat:?}"
        );
        thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(
        blind_source(&hero),
        BlindSource::Related,
        "#1409 a zombie leader with a live group has not released its descriptors"
    );
    drop(kill_on_drop);
    // Wait for the group to be gone before dropping the window: releasing it
    // while the group is still dying would hand a concurrent scan a source
    // that settles into `Terminal` only after its own deadline. The wait also
    // proves the guard's kill landed. The probe's own `waitpid` reaps the
    // child as soon as it dies, so "gone from /proc" is the usual exit — a
    // vanished entry cannot be a source at all. The timeout branch kills the
    // group explicitly and confirms the kill before it panics (#2142 fix-r2,
    // sibling r2 #3 / concurrency r2 #2 — the same shape the EOF test's
    // window has kept since fix-r1; fix-r3, concurrency P1, made the
    // confirmation an asserted escalation instead of a dropped verdict), so
    // no exit releases the window over a live group.
    if !group_is_gone_within(&hero, PROBE_KILL_WINDOW) {
        confirm_probe_group_death(group, &hero);
        panic!("#1409 the probe's thread group must die by the guard's kill");
    }
    // #2142 fix-r1 (concurrency r1 #2; comment corrected in fix-r2, sibling r2
    // #3 / concurrency r2 #2): drop the group guard while the probe leader —
    // the python parent, whose own pid is the pgid — is still alive (blocked
    // in its `os.waitpid`, or just woken from it): a pgid cannot be recycled
    // while its leader lives, so the kill can never land on an unrelated
    // group that reused the id. The parent's `os.waitpid` is also what
    // usually reaps the hero, whichever exit the wait above took; when the
    // kill reaches it first, the hero is reparented and the bounded
    // `reap_probe_orphan` below collects it whenever it lands on this
    // process. Group first, window last, the same order the unwind takes.
    drop(_probe_group);
    drop(_scan_window);
    let _ = probe.wait().expect("#1409 reap the zombie-leader probe");
    reap_probe_orphan(hero_raw);
}

/// #2154 (probe OOM window): the zombie-leader probe killed between its
/// `os.fork()` and printing the child's pid is the stranding shape — the read
/// sees EOF where a pid should be, the expect on the parse panics, and before
/// the group guard that panic unwound with nothing owning the probe's child:
/// the poison state (zombie leader, live thread group, EACCES fd directory)
/// outlived both the test and the released [`SCAN_WINDOW`] for as long as the
/// surviving thread slept. This test simulates exactly that path — the probe
/// parent exits without printing, and the forked child closes stdout
/// (otherwise EOF waits on the child's inherited copy) and parks itself in
/// the poison state, publishing its pid to a receipt instead of the pipe —
/// and asserts the guard's kill takes the whole group with it: poison may not
/// outlive the probe. The surviving thread sleeps for `PROBE_HOLD_SECS` —
/// the same cap the real leader probe's survivor sleeps — and that is what
/// makes the kill-detection margin structural (#2142 fix-r1, concurrency r1
/// #1 / boundary r1 #2; fix-r2 closed the last gap, boundary r2 #1 / MM
/// boundary r2 #1): every stretch of path ahead of the kill window is
/// bounded by its own assert (4s for the pid line — read on a worker
/// thread, the one wait `read_line` gives no deadline of its own — 10s for
/// the receipt, 10s for the zombie, 5s for the window = 29s), each budget
/// failing red before it can silently overrun, and the sum is a
/// compile-time fact (`PROBE_HOLD_SECS` and the margin assert beside it),
/// so the kill-removed mutant cannot be outwaited by the front loops' own
/// clocks — where a 10s hold left the verdict to host speed. The timeout
/// branch additionally kills the group explicitly and confirms the kill
/// before it panics — an asserted escalation since #2142 fix-r3 (concurrency
/// P1: one more kill and a named retry window when the first confirm has not
/// landed, so a group that outlives its own SIGKILL fails the assert instead
/// of seeing the panic release the window over it) — so even the mutant
/// leaves no residue past its failure and never releases the window over a
/// live group.
#[cfg(target_os = "linux")]
#[test]
fn a_probe_that_dies_before_the_pid_line_takes_its_group_with_it() {
    let _scan_window = SCAN_WINDOW
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let scratch = Scratch::new("zombie-leader-eof");
    let receipt = scratch.path().join("child.pid");
    // #2142 fix-r2: the hold cap is the shared `PROBE_HOLD_SECS` (no second
    // handwritten 30 to drift), and the child publishes its pid through a
    // staging name and one rename (MM concurrency r2 #4) — the test polls the
    // receipt "exists means readable", and a created-but-empty receipt could
    // parse a half-written pid and watch the wrong group.
    let probe_script = format!(
        r#"
import ctypes, os, sys, threading, time
receipt = sys.argv[1]
pid = os.fork()
if pid == 0:
    os.close(1)
    def hold():
        time.sleep({hold})
    threading.Thread(target=hold, daemon=False).start()
    time.sleep(0.2)
    staged = receipt + ".staging"
    with open(staged, "w") as out:
        out.write(str(os.getpid()))
    os.rename(staged, receipt)
    ctypes.CDLL("libc.so.6", use_errno=True).pthread_exit(None)
    os._exit(9)
os._exit(9)
"#,
        hold = PROBE_HOLD_SECS
    );
    let mut probe = process::Command::new("python3")
        .arg("-I")
        .arg("-c")
        .arg(&probe_script)
        .arg(&receipt)
        .stdout(process::Stdio::piped())
        .process_group(0)
        .spawn()
        .expect("#2154 spawn the EOF probe");
    let group = Pid::from_raw(probe.id() as i32).expect("#2154 the EOF probe's pid must be valid");
    let _probe_group = KillProbeGroupOnDrop(group);
    // #2142 fix-r2 (boundary r2 #1 / MM boundary r2 #1): the pid line is read
    // on a worker thread with its own budget — this read used to be the one
    // unbounded wait ahead of the 10/10/5 budgets, leaving the kill margin
    // at the hold cap's mercy.
    let (read, pid_line) = read_probe_line(
        probe
            .stdout
            .take()
            .expect("#2154 take the EOF probe's stdout"),
        PROBE_LINE_BUDGET,
    );
    assert_eq!(
        read, 0,
        "#2154 the probe died before the pid line, so the read is an EOF, not {pid_line:?}"
    );
    // The child publishes its pid to the receipt — the pid line it never got
    // to write. The receipt replaces nothing about the pipe: the panic path
    // under repair had no pid at all, and this is only how the test learns
    // which pid was stranded.
    let deadline = Instant::now() + PROBE_RECEIPT_BUDGET;
    let hero_raw = loop {
        if let Some(pid) = fs::read_to_string(&receipt)
            .ok()
            .and_then(|text| text.trim().parse::<i32>().ok())
        {
            break pid;
        }
        assert!(
            Instant::now() < deadline,
            "#2154 the stranded child must publish its pid to {receipt:?}"
        );
        thread::sleep(Duration::from_millis(5));
    };
    let hero = hero_raw.to_string();
    let deadline = Instant::now() + PROBE_ZOMBIE_BUDGET;
    loop {
        let stat = fs::read_to_string(format!("/proc/{hero}/stat")).unwrap_or_default();
        if matches!(stat_fields(&stat), Some(('Z', _))) {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "#2154 the stranded leader must become a zombie, not stay {stat:?}"
        );
        thread::sleep(Duration::from_millis(5));
    }
    // The guard's kill is what ends the poison state; without it the group
    // lives for the hold thread's full 30s. The drop order here mirrors the
    // unwind's: group first, window last — and the window is released only
    // behind a group that is provably dying: the timeout branch kills
    // explicitly, confirms the kill inside its own bound, and only then
    // panics (#2142 fix-r2, concurrency r2 #2 — the panic's unwind releases
    // the window, so it must not overtake the kill it just issued; fix-r3,
    // concurrency P1, made that confirmation an asserted escalation — a
    // second kill and a named retry window — rather than a dropped verdict).
    drop(_probe_group);
    if !group_is_gone_within(&hero, PROBE_KILL_WINDOW) {
        // #2142 fix-r1 (concurrency r1 #1 / boundary r1 #2): the mutant's
        // on-disk residue is bounded right here — the failure alone would
        // leave the poison group alive for the hold thread's full 30s — and
        // the kill lives in `confirm_probe_group_death`, whose line still
        // reads differently from the guard's Drop line the mutant removes
        // (`group` and `.ok()`, not `self.0` and `let _ =`), so the
        // negative-control anchor stays unique.
        confirm_probe_group_death(group, &hero);
        panic!("#2154 the poison group must not outlive the guard's kill");
    }
    drop(_scan_window);
    let _ = probe.wait().expect("#2154 reap the EOF probe");
    // #2142 fix-r1 (concurrency r1 #3; fix-r2 bounded it, MM concurrency r2
    // #1 / boundary r2 #3): the probe parent never waits for its child, so
    // the hero lands on whoever holds the child-subreaper flag — a concurrent
    // stranger guard's lease parks it on this process, and the file's rule is
    // that every guard reaps its own. Collect it when it is ours, in the
    // bounded `NOHANG` shape every other wait here keeps; ECHILD means init
    // owns it and has already collected it.
    reap_probe_orphan(hero_raw);
}

/// #1409: the one rule the three `read_link` sites share, pinned on its own —
/// `ENOENT` proves the release, any other errno leaves a descriptor the scan
/// cannot see, and only the source settles it: this process and a path with
/// no numeric pid keep it live, a holder that no longer exists releases it.
#[cfg(target_os = "linux")]
#[test]
fn an_unreadable_link_is_a_release_only_when_its_holder_is_gone() {
    let eacces = io::Error::from_raw_os_error(13);
    let mine = format!("/proc/{}/fd/0", process::id());
    assert!(
        !link_failure_is_a_release(Path::new(&mine), &eacces, &mut BlindSources::default()),
        "#1409 an unreadable link of this process must never count as a release"
    );
    assert!(
        !link_failure_is_a_release(
            Path::new("/proc/self/fd/0"),
            &eacces,
            &mut BlindSources::default()
        ),
        "#1409 a path with no numeric pid has an unknown holder, not a release"
    );
    assert!(
        link_failure_is_a_release(
            Path::new(&mine),
            &io::Error::from(io::ErrorKind::NotFound),
            &mut BlindSources::default()
        ),
        "#1409 a link that is gone is a release whatever its holder is"
    );
    assert!(
        link_failure_is_a_release(
            Path::new("/proc/2147483647/fd/0"),
            &eacces,
            &mut BlindSources::default()
        ),
        "#1409 an unreadable link behind a holder that does not exist is a release"
    );
}

/// #2142 fix-r3 (concurrency #1): the error side of the group-is-gone wait,
/// pinned the same way the link rule above is — a healthy host's `/proc`
/// never answers the error side for a descendant, so the classification is
/// the testable seam. Only a vanished entry is a disappearance: a procfs
/// that hides the hero behind EACCES (hidepid, a restricted container) must
/// leave the group counted as alive, or the wait confirms a death it never
/// observed — the fail-open that could have false-PASSED the kill-removed
/// mutant.
#[cfg(target_os = "linux")]
#[test]
fn a_proc_read_error_means_gone_only_when_the_entry_is_absent() {
    assert!(
        metadata_failure_is_gone(&io::Error::from(io::ErrorKind::NotFound)),
        "#2142 a vanished /proc entry is the one read error that means the group is gone"
    );
    assert!(
        !metadata_failure_is_gone(&io::Error::from_raw_os_error(13)),
        "#2142 an unreadable /proc entry means the group's state is unknown, never gone"
    );
    assert!(
        !metadata_failure_is_gone(&io::Error::from_raw_os_error(5)),
        "#2142 an I/O error reading /proc means the group's state is unknown, never gone"
    );
}

/// #1409: the positive self-check must hold on a healthy Linux host — the
/// scan sees a write end this process holds, through the same primitives it
/// uses on every other process.
#[cfg(target_os = "linux")]
#[test]
fn the_scan_sees_a_write_end_this_process_holds() {
    a_write_end_this_process_holds_is_visible().expect(
        "#1409 the scan must see its own write end; this host's /proc hides it, so a clean \
         scan would prove nothing",
    );
}

/// #1409 item 3: the grace re-check evicts a candidate whose fd no longer
/// resolves to the pipe — the release a scan would otherwise report as a
/// surviving write end — and keeps the one that still does.
#[cfg(target_os = "linux")]
#[test]
fn a_candidate_that_no_longer_resolves_is_evicted_by_the_grace() {
    use std::os::fd::AsRawFd;
    let (_reader, writer) = io::pipe().expect("#1409 create the grace probe pipe");
    let inode = rustix::fs::fstat(writer.as_fd())
        .expect("#1409 fstat the grace probe pipe")
        .st_ino;
    let wanted = format!("pipe:[{inode}]");
    let live = format!("/proc/self/fd/{}", writer.as_raw_fd());
    let gone = "/proc/self/fd/1048576".to_owned();
    let mut candidates = vec![live.clone(), gone];
    retain_through_grace(
        &mut candidates,
        &wanted,
        Duration::from_millis(200),
        &mut BlindSources::default(),
    );
    assert_eq!(
        candidates,
        vec![live],
        "#1409 the grace must evict a candidate that no longer resolves and keep the fd that does"
    );
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

/// #1155 fix-r3 review P1-1: publish the resume signal the same atomic way,
/// so a helper parked at its paused poll wakes on a complete file, never on
/// a partially written one.
fn publish_resume_signal(resume: &Path) {
    let staging = resume.with_extension("staging");
    fs::write(&staging, b"resume").expect("#1155 write the resume signal's staging file");
    fs::rename(&staging, resume).expect("#1155 publish the resume signal");
}

/// #1297: the release signal's path, `started + ".release"` — exactly how the
/// stranger derives it in [`STRANGER_PROBE`] (it receives the started path and
/// nothing else it could derive a second signal from), and built from the
/// path's own bytes so the two sides cannot disagree. Publishing this file is
/// what makes the stranger's `started` event-driven: the test, not a fixed
/// sleep on the stranger's clock, decides when the handshake completes.
fn release_signal(started: &Path) -> PathBuf {
    let mut release = started.as_os_str().to_os_string();
    release.push(".release");
    PathBuf::from(release)
}

/// #1297: publish the release signal atomically, staging + rename (#166 r8),
/// like the go and resume signals. The staging name is `release + ".staging"`
/// by string concatenation — mirroring the stranger's own python `path +
/// ".staging"` — so it never reuses the `started` signal's staging name the
/// way `with_extension` would ("stranger-started.release" with its extension
/// replaced becomes "stranger-started.staging").
fn publish_release_signal(release: &Path) {
    let staging = {
        let mut staging = release.as_os_str().to_os_string();
        staging.push(".staging");
        PathBuf::from(staging)
    };
    fs::write(&staging, b"release").expect("#1297 write the release signal's staging file");
    fs::rename(&staging, release).expect("#1297 publish the release signal");
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
