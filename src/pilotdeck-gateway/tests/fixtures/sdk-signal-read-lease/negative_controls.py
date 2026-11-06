#!/usr/bin/env python3
# anchor-selfcheck: unsupported two-phase concurrency lease control, no source replacement table
"""#1662 paired read-lease control for the resident sdk_signal_exit golden test.

RED proves the #1662 false FAIL: with the fixture read lease's contended wait
removed, the resident ``signal_oracle_matches_recorded_source_and_local_files``
test reads a committed-mode mutant of ``capture.py`` and fails the golden
sha256 while the source guard still holds the tree lock. GREEN proves the fixed
test survives the same writer protocol: an independent writer child holds the
tree lock with the mutant on disk, the fixed test waits for the lease, and
reads only restored bytes. Both verdicts are published after every source was
restored and re-verified; any non-KILLED row exits non-zero.

The two directions use separate judges. In RED a golden-sha failure is the
mutant being killed; in GREEN only a *pass* counts, and the pass must carry the
resident test's own measured lease wait (``#1662 read lease wait ms=``) at or
above ``MIN_LEASE_WAIT_MS`` -- a pass without it never met the writer and
proves nothing. GREEN failures of any kind are INVALID, never KILLED.

The writer publishes the exact interval it holds the lock in; the resident test
reports the instant of its *first* lock attempt, and GREEN requires that instant
to fall inside the interval. A wait measured against some later lock holder, or
a stall after an uncontended acquisition, is therefore INVALID, never KILLED.

Run only after commit (mutating_sources refuses a dirty tree):
    CARGO_TARGET_DIR=<target> python3 -I .../sdk-signal-read-lease/negative_controls.py NEW_OUTPUT_DIR
"""
import json
import os
import re
import signal
import subprocess
import sys
import time
from pathlib import Path

sys.dont_write_bytecode = True
sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from negative_process import (
    _collect,
    _ensure_workspace_scanner,
    _head_bytes,
    _kill_group,
    exclusive_write,
    mutating_sources,
    new_output_directory,
    run_owned,
    wait_for_slot,
)
from negative_suite import passed, publish_summary
from negative_panic import named_test_failed, panic_evidence

root = Path(__file__).resolve().parents[5]
test_source = root / 'src/pilotdeck-cli/tests/sdk_signal_exit.rs'
capture = root / 'src/pilotdeck-cli/tests/fixtures/sdk-signals/capture.py'
wrapper = root / 'src/pilotdeck-cli/tests/fixtures/sdk-signals/sigint_default.py'
RESIDENT_TEST = 'signal_oracle_matches_recorded_source_and_local_files'
SHA_MARKER = '#1145 source file SHA256 matches disk: probeSha256/capture.py'
# The contended-wait sleep is the whole #1662 fix: returning the file instead
# keeps reading guarded bytes while the committed writer still holds the lock.
LEASE_WAIT_ANCHOR = 'thread::sleep(Duration::from_millis(50));'
LEASE_WAIT_REMOVED = 'return file;'
CAPTURE_RESIDUE = '\n# 1662 committed-mode mutant residue\n'
# How long the writer holds the lock once its window opens. The reader is
# launched right after the window appears (its cargo slot is taken beforehand),
# so it reaches the lock well inside this hold. The writer also publishes the
# exact hold interval in epoch milliseconds; GREEN requires the reader's own
# acquisition timestamp to fall inside it, which is what binds the witness to
# *this* writer instead of any later lock holder (#1662 review r3 P0).
HOLD_SECONDS = 90
WRITER_HOLD = 'writer-hold.json'
# The writer fails fast instead of queueing behind a long committed suite: it
# only has to win the lock once, and a slow acquisition is a busy workspace,
# not evidence about the lease. 300 s stays well inside the 900 s default other
# devices use, so this control never becomes the longest lock holder.
WRITER_LOCK_TIMEOUT = 300
# Writer start -> lock acquisition (<= WRITER_LOCK_TIMEOUT) -> window open.
WRITER_ENTRY_BUDGET = WRITER_LOCK_TIMEOUT + 300
# Writer finish after the hold: the rest of the window plus the restore.
WRITER_EXIT_BUDGET = HOLD_SECONDS + 120
# The resident test reports its own lease wait on stderr (cargo test only shows
# it under --nocapture). That measurement is the contention witness: elapsed
# cargo time also covers startup, the package-cache lock and rebuilds, so it can
# be large without any lease contention (and small after a real, short wait).
# Any wait at all proves the reader met the writer; 500 ms is far above the
# 50 ms poll granularity and far below a window-scale wait.
LEASE_MARKER = '#1662 read lease wait ms='
MIN_LEASE_WAIT_MS = 500
# ``--stop-check`` only exercises the writer's exit contract, so it can use a
# short hold: nothing waits on the lease there.
STOP_CHECK_HOLD_SECONDS = 5
# ``epoch_ms()`` truncates, so a timestamp is a whole millisecond *bucket*, not
# an instant: the writer's ``end_ms`` bucket also covers the release, and a
# reader refused right after that release by the *next* exclusive holder could
# still report the same number. GREEN therefore requires the refusal to sit at
# least this far inside the published interval. One second is ~1 % of the hold
# and far above any scheduling slop, yet it makes ``blocked <= end_ms - margin``
# mean "refused at least a second before this writer let go" -- while the writer
# demonstrably still held the exclusive lock (#1662 review r8 P0).
WITNESS_MARGIN_MS = 1000
CARGO_TIMEOUT = 1800


class _Stopped(Exception):
    """The writer was asked to stop; its unwind restored and re-verified first.

    Raised from the SIGTERM handler and caught around ``mutating_sources``, so a
    requested stop is a *success* (exit 0) while any failure of its own -- a
    restore or a HEAD re-read that raises -- still exits non-zero and is
    reported by the controller (#1662 review r8 P1).
    """


def cargo_argv(suffix):
    return [sys.executable, '-I', '-B', str(wrapper), 'cargo', 'test', '--offline',
            '--manifest-path', 'src/Cargo.toml', '-p', 'pilotdeck-cli',
            '--test', 'sdk_signal_exit', *suffix]


def run_cargo(out, label, suffix, slot_taken=False):
    """Run one cargo invocation; return ``(result, seconds spent inside it)``.

    The elapsed time is reported for the evidence trail only; it is not a
    contention witness (see ``MIN_LEASE_WAIT_MS``). Waiting for a cargo slot is
    excluded either way: it is a scheduling delay, not lease contention.
    ``slot_taken`` skips the wait when the caller already took one (GREEN does
    that before the writer opens its window, so the hold only has to cover the
    test itself).
    """
    if not slot_taken:
        slot = wait_for_slot(root / 'scripts/cargo-slots.sh', timeout=900, interval=15)
        exclusive_write(out / f'{label}.slots.log', slot.stdout + slot.stderr)
    started = time.monotonic()
    try:
        result = run_owned(cargo_argv(suffix), cwd=root, env=os.environ.copy(),
                           timeout=CARGO_TIMEOUT)
    except subprocess.TimeoutExpired as error:
        # Keep whatever was observed: an audit must be able to tell "never
        # started" from "started and was killed at the bound".
        exclusive_write(out / f'{label}.timeout.stdout', error.stdout or '')
        exclusive_write(out / f'{label}.timeout.stderr', error.stderr or '')
        raise
    elapsed = time.monotonic() - started
    exclusive_write(out / f'{label}.stdout', result.stdout)
    exclusive_write(out / f'{label}.stderr', result.stderr)
    return result, elapsed


def publish(path, text):
    """Write a rendezvous marker atomically: readers see the file or nothing."""
    temporary = path.with_name(path.name + '.tmp')
    temporary.write_text(text)
    temporary.replace(path)


def epoch_ms():
    return int(time.time() * 1000)


def publish_hold(out, start_ms, end_ms):
    """Publish the writer's hold interval; ``end_ms`` is None while it is open."""
    publish(out / WRITER_HOLD, json.dumps({'start_ms': start_ms, 'end_ms': end_ms}) + '\n')


def read_hold(out):
    hold = json.loads((out / WRITER_HOLD).read_text())
    return int(hold['start_ms']), hold['end_ms'] and int(hold['end_ms'])


def read_released(out):
    """``(released_ms, restored)`` after the real release, or None if never published.

    ``restored`` is the writer's *direct observation* that the guarded file is
    back to its HEAD bytes at the moment it recorded the release -- not an
    inference from the exit status, and not implied by the record's existence.
    The controller checks it (#1662 review r8 P2).
    """
    path = out / 'writer-released.json'
    if not path.is_file():
        return None
    record = json.loads(path.read_text())
    return int(record['released_ms']), bool(record.get('restored'))


def capture_matches_head():
    """Is the guarded file byte-identical to its HEAD version right now?"""
    try:
        return capture.read_bytes() == _head_bytes(root, capture.relative_to(root), 'HEAD')
    except Exception:
        # Unreadable HEAD or unreadable file: no restoration claim either way.
        return False


def writer(out, hold):
    """Independent committed-mode writer: hold the tree lock over one mutant.

    The hold interval is published in epoch milliseconds (``writer-hold.json``,
    written once when the window opens and again just before the lock is
    released). GREEN compares the reader's own acquisition timestamp against
    that interval, so a wait measured against some later lock holder cannot be
    mistaken for this writer (#1662 review r3 P0).

    SIGTERM is acted on only while the mutation window is open, and the handler
    disarms *itself* before raising: the unwind then restores every guarded byte
    with the handler already off, so a second SIGTERM landing inside that
    restore cannot interrupt ``write_bytes`` (#1662 review r3 P0). A SIGKILL (or
    a failing restore) can still leave the mutant on disk; the next
    ``mutating_sources`` run then refuses the dirty tree instead of silently
    mutating a mutated baseline.

    A requested stop exits 0: the handler raises a private ``_Stopped`` and the
    unwind through ``mutating_sources.__exit__`` restores and re-reads every
    guarded byte before the exception reaches the handler here. Every other way
    out -- a failing restore, an unreadable HEAD, a signalling bug -- still exits
    non-zero, which is what lets the controller judge the exit status
    unconditionally (#1662 review r8 P1).
    """
    armed = [True]
    entered = [False]

    def stop(_signum, _frame):
        if armed[0]:
            armed[0] = False
            # A private exception, not ``SystemExit``: the controller must be able
            # to tell "we asked it to stop, it restored and left cleanly" from
            # "it failed on its own". Only the former exits 0 (#1662 r8 P1).
            raise _Stopped('#1662 writer stopped before its window closed')

    signal.signal(signal.SIGTERM, stop)
    try:
        try:
            with mutating_sources(root, [capture], lock_timeout=WRITER_LOCK_TIMEOUT):
                try:
                    capture.write_text(capture.read_text() + CAPTURE_RESIDUE)
                    # Publish the window atomically: a reader that sees the file must
                    # also see the mutant bytes (#166 tmp-and-rename).
                    opened = epoch_ms()
                    publish_hold(out, opened, None)
                    publish(out / 'writer-window-open', 'open\n')
                    # Only now is the window *published*: before this point a
                    # SIGTERM leaves a "released" record with no window behind it,
                    # which is exactly the fake evidence r7 removed (#1662 r8 P3).
                    entered[0] = True
                    time.sleep(hold)
                    # Recorded while the lock is still held: the reader can only
                    # acquire it after this instant.
                    publish_hold(out, opened, epoch_ms())
                    if os.environ.get('PDRS_1662_WRITER_FAIL_AFTER_HOLD'):
                        # Negative-control hook for ``--stop-check``: a writer that
                        # fails on its own must exit non-zero, so the controller's
                        # unconditional exit-status rule can be shown to fire
                        # (#1662 review r8 P1).
                        raise RuntimeError('#1662 injected writer failure after the hold')
                finally:
                    # Disarm on every path out of the window, including an exception:
                    # restoration must never run with the handler still armed.
                    armed[0] = False
        except _Stopped as stopped:
            # ``mutating_sources.__exit__`` restored every guarded byte, re-read
            # it and only then released the lock -- the stop request is fulfilled
            # exactly as designed, so it is not a failure: exit 0.
            print(stopped, file=sys.stderr)
    finally:
        # ``mutating_sources.__exit__`` has restored every guarded byte and
        # released the lock by now, so this instant is the earliest another
        # process can acquire it. Published on the normal path *and* on the
        # SIGTERM unwind, so each run can show, rather than argue, that the
        # recorded ``end_ms`` lies strictly inside the real hold
        # (#1662 review r7 arbitration evidence). Only when the window was
        # actually published: a SIGTERM before that never held a published
        # window, and a "released" record there would be fake evidence
        # (#1662 review r7 P3 / r8 P3). ``restored`` is measured here, not
        # assumed: the record itself says whether the bytes are back (#1662 r8 P2).
        if entered[0]:
            publish(out / 'writer-released.json',
                    json.dumps({'released_ms': epoch_ms(),
                                'restored': capture_matches_head()}) + '\n')
    return 0


def _drain(process, out):
    """Best effort: read and close a writer's pipes so nothing leaks.

    ``communicate()`` keeps whatever the writer printed; the point is that the
    streams are closed here rather than at GC time, and that a full pipe cannot
    leave the writer blocked in ``write`` (#1662 review r8 P3). Any failure is
    swallowed: this runs on an error path and must not mask the stop verdict.
    """
    try:
        stdout, stderr = _collect(process, 30)
        exclusive_write(out / 'writer.force-stop.stdout', stdout or '')
        exclusive_write(out / 'writer.force-stop.stderr', stderr or '')
    except Exception:
        pass


def force_stop(process, out):
    """Recover when ``stop_writer`` itself was interrupted.

    Graceful first (the writer's SIGTERM handler unwinds ``mutating_sources``,
    which restores every guarded byte), then a bounded wait, and only then a
    kill -- a kill during restoration can leave a mutant or a truncated file on
    disk (#1662 review r5/r6). Returns True when the writer exited on its own,
    which is what makes the restoration claim true. A requested stop exits 0
    (#1662 review r8), but this path stays about *whether* it stopped: the
    caller of ``force_stop`` only has a fallback, and a non-zero status here is
    judged by ``stop_writer``'s own contract, not swallowed.
    """
    if process.poll() is not None:
        _drain(process, out)
        return True
    try:
        process.terminate()
        process.communicate(timeout=WRITER_EXIT_BUDGET)
        # It exited: a requested stop restores and then leaves with status 0, so
        # the status is judged by ``stop_writer``; here only *that* it stopped
        # matters.
        return True
    except subprocess.TimeoutExpired:
        pass
    except Exception:
        # Something other than a timeout (an IPC error, a closed pipe). The
        # writer may still be restoring, so poll it to the same bound instead of
        # killing it outright; ``poll()`` cannot raise the way ``wait()`` can, so
        # this path still ends in a *confirmed* timeout before any kill
        # (#1662 review r6/r7 P0-2).
        deadline = time.monotonic() + WRITER_EXIT_BUDGET
        while process.poll() is None and time.monotonic() < deadline:
            time.sleep(0.5)
        if process.poll() is not None:
            _drain(process, out)
            return True
    try:
        _kill_group(process)
    finally:
        # Always reap, even when the kill itself fails.
        _collect(process, 30)
    return False


def stop_writer(process, out):
    """Bounded stop: let the writer restore first, then terminate, then kill.

    The exit status is judged unconditionally: a writer we terminated ourselves
    restores and then exits 0, so anything else -- a restore that raised, a HEAD
    re-read that failed, a status set before our signal ever landed -- is a real
    failure and must be reported (#1662 review r8 P1). Only a kill we performed
    is exempt, and that raises on its own.
    """
    killed = False
    try:
        stdout, stderr = process.communicate(timeout=WRITER_EXIT_BUDGET)
    except subprocess.TimeoutExpired:
        # Graceful first: the handler unwinds mutating_sources and restores.
        process.terminate()
        try:
            stdout, stderr = process.communicate(timeout=WRITER_EXIT_BUDGET)
        except subprocess.TimeoutExpired:
            killed = True
            _kill_group(process)
            stdout, stderr = _collect(process, 30)
    exclusive_write(out / 'writer.stdout', stdout or '')
    exclusive_write(out / 'writer.stderr', stderr or '')
    if killed:
        raise RuntimeError('#1662 writer had to be killed; '
                           'source restoration is NOT confirmed')
    if process.returncode != 0:
        raise RuntimeError(f'#1662 writer exited {process.returncode}')


def judge(result):
    log = result.stdout + result.stderr
    evidence = panic_evidence(log, RESIDENT_TEST)
    if (result.returncode != 0 and named_test_failed(log, RESIDENT_TEST)
            and 'test result: FAILED' in log and 'could not compile' not in log
            and SHA_MARKER in evidence):
        return 'KILLED', 'resident test failed at its golden sha assertion'
    if passed(result, RESIDENT_TEST):
        return 'SURVIVED', 'resident test still passed'
    return 'INVALID', 'missing bound runtime failure (build error, wrong panic, or missing test)'


def observations(log):
    """Lease acquisitions the resident test reported.

    Each row is ``(wait ms, first attempt, first refused attempt, acquisition)``
    in epoch milliseconds; ``blocked`` is 0 when the first attempt succeeded.
    """
    return [(int(waited), int(first), int(blocked), int(at))
            for waited, first, blocked, at in
            re.findall(re.escape(LEASE_MARKER)
                       + r'(\d+) first=(\d+) blocked=(\d+) at=(\d+)', log)]


def green(out):
    """Fixed test under the writer protocol: wait for the lease, read restored bytes.

    GREEN has its own verdicts on purpose. ``judge()`` is the RED direction: a
    golden-sha failure there means the mutant killed the test. Here the same
    failure means the fix stopped working, so reusing ``judge()`` would report
    a broken lease as a passing control (#1662 review P0). Only a pass counts,
    and only when the test itself reports a lease wait.
    """
    prewarm, _elapsed = run_cargo(out, 'prewarm-compile', ['--no-run'])
    if prewarm.returncode != 0:
        return 'INVALID', (f'prewarm compile failed (rc={prewarm.returncode}); the '
                           f'timed run would rebuild and its timing means nothing')
    # Build the #1166 workspace scanner outside the writer's budget: otherwise
    # mutating_sources builds it inside WRITER_ENTRY_BUDGET on a cold target.
    _ensure_workspace_scanner()
    # Field-family drift guard: the marker is written by the Rust test and read
    # here, so a rename on either side would silently turn every GREEN into
    # INVALID (fail-closed, but the control would quietly stop working).
    if LEASE_MARKER not in test_source.read_text():
        return 'INVALID', (f'marker {LEASE_MARKER!r} is no longer in '
                           f'{test_source.name}; the two sides drifted apart')
    # Take the cargo slot *before* the writer opens its window: the slot wait is
    # the only unbounded delay between the window and the test's first attempt
    # at the lock, and HOLD_SECONDS has to cover it (#1662 review r3 P3).
    slot = wait_for_slot(root / 'scripts/cargo-slots.sh', timeout=900, interval=15)
    exclusive_write(out / 'green.slots.log', slot.stdout + slot.stderr)
    process = subprocess.Popen(
        [sys.executable, '-I', str(Path(__file__).resolve()), '--writer', str(out),
         str(HOLD_SECONDS)],
        stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True,
        start_new_session=True)
    failure = None
    stop_error = None
    hard_interrupt = None
    result = None
    try:
        deadline = time.monotonic() + WRITER_ENTRY_BUDGET
        while not (out / 'writer-window-open').is_file():
            if process.poll() is not None:
                raise RuntimeError('#1662 writer exited before opening its window')
            if time.monotonic() > deadline:
                raise TimeoutError('#1662 writer never opened its mutation window')
            time.sleep(0.5)
        result, _elapsed = run_cargo(out, 'fixed-test-survives-writer-window',
                                     [RESIDENT_TEST, '--', '--exact', '--nocapture'],
                                     slot_taken=True)
    except BaseException as error:
        failure = error
    finally:
        # ``finally``, not a sequential call: BaseException (KeyboardInterrupt,
        # SystemExit) must stop the writer too, or it outlives us holding the
        # lock with the mutant on disk (#1662 review r3 P1).
        try:
            stop_writer(process, out)
        except BaseException as error:
            stop_error = error
            # Keep the interrupt itself: rewriting ``stop_error`` below must not
            # turn a Ctrl-C into a plain INVALID row (#1662 review r6).
            if not isinstance(error, Exception):
                hard_interrupt = error
            # The stop itself can be interrupted (SIGINT during communicate).
            # Recover gracefully and only kill as a last resort; ``stopped``
            # records whether we can still claim the sources were restored.
            fallback_note = ''
            try:
                stopped = force_stop(process, out)
            except BaseException as stop_failure:
                # The fallback itself failed (kill or reap raised). Keep both
                # errors and, above all, still re-raise an interrupt below
                # instead of losing it here (#1662 review r7 P1).
                stopped = False
                if not isinstance(stop_failure, Exception):
                    if hard_interrupt is None:
                        hard_interrupt = stop_failure
                else:
                    # An ordinary failure of the fallback is not an interrupt to
                    # re-raise, but it must not vanish either: the audit trail is
                    # the only place the operator can see why the stop is
                    # unconfirmed (#1662 review r8 P3).
                    fallback_note = (f'; the stop fallback itself failed: '
                                     f'{type(stop_failure).__name__}: {stop_failure}')
            if not stopped:
                stop_error = RuntimeError(
                    f'{type(error).__name__}: {error}; the writer had to be killed or '
                    f'could not be stopped, source restoration is NOT confirmed'
                    f'{fallback_note}')
    for interrupt in (failure, hard_interrupt):
        if isinstance(interrupt, BaseException) and not isinstance(interrupt, Exception):
            # Interrupts still abort the run; the writer is already stopped above.
            raise interrupt
    if failure is not None or stop_error is not None:
        # Report both: a stop failure must not hide the primary reason (and the
        # other way round) -- the summary is the audit trail (#1662 review r3 P2).
        parts = []
        if failure is not None:
            parts.append(f'{type(failure).__name__}: {failure}')
        if stop_error is not None:
            parts.append(f'stop failed: {type(stop_error).__name__}: {stop_error}')
        return 'INVALID', '; '.join(parts)
    log = result.stdout + result.stderr
    if not passed(result, RESIDENT_TEST):
        return 'INVALID', (
            f'fixed test did not survive the writer window (rc={result.returncode}, '
            f'golden-sha failure={SHA_MARKER in panic_evidence(log, RESIDENT_TEST)}); '
            f'GREEN only accepts the lease wait followed by a pass')
    waits = observations(log)
    if not waits:
        return 'INVALID', (f'resident test reported no "{LEASE_MARKER}" line, so it never '
                           f'waited on the writer lease; this row is no evidence for the fix')
    start_ms, end_ms = read_hold(out)
    if end_ms is None:
        return 'INVALID', ('the writer never published the end of its hold; the acquisition '
                           'timestamp cannot be attributed to it')
    # The witness is the *refused* attempt: ``try_lock_shared`` returning
    # WouldBlock is a direct observation that an exclusive holder was there, and
    # nothing but a real holder produces it. Requiring that instant to fall
    # inside this writer's hold leaves no room for a scheduler pause (before the
    # call, or after an uncontended acquisition) and none for a later lock
    # holder (#1662 review r4/r5 P0). It must fall ``WITNESS_MARGIN_MS`` inside
    # both ends, because the timestamps are truncated milliseconds: see the
    # constant for the boundary path that margin closes (#1662 review r8 P0).
    waited, first, blocked, at = max(waits, key=lambda row: row[0])
    if blocked == 0:
        return 'INVALID', ('the resident test never observed a refused attempt, so it never '
                           'met an exclusive holder; this row is no evidence for the lease')
    if not (start_ms + WITNESS_MARGIN_MS <= blocked <= end_ms - WITNESS_MARGIN_MS):
        return 'INVALID', (f'resident test was refused at {blocked} ms, outside the writer '
                           f'hold [{start_ms}, {end_ms}] less the {WITNESS_MARGIN_MS} ms '
                           f'witness margin: that refusal was not this writer')
    released = read_released(out)
    if released is None:
        # The writer never published the instant it released the lock, so the
        # hold interval cannot be shown to be conservative. Missing evidence is
        # not evidence (#1662 review r7 P1).
        return 'INVALID', ('the writer never published writer-released.json; the hold '
                           'interval cannot be shown to end before the real release')
    released_ms, restored = released
    if not restored:
        # The record exists but the writer's own restore/HEAD re-read did not
        # complete: the lock was dropped, the bytes may not be back. A record
        # alone is not a restoration claim (#1662 review r8 P2).
        return 'INVALID', (f'writer released the lock at {released_ms} ms but did not '
                           f'confirm that the guarded sources were restored')
    if released_ms <= end_ms:
        # Would mean the writer released the lock at or before the instant it
        # recorded as inside its hold; the whole boundary argument rests on this
        # being false, so refuse to publish a verdict if it ever is not.
        return 'INVALID', (f'writer recorded end_ms={end_ms} but released at '
                           f'{released_ms} ms; the hold interval is not conservative')
    if not first <= blocked <= at:
        return 'INVALID', (f'inconsistent marker: first={first} blocked={blocked} at={at}')
    if waited < MIN_LEASE_WAIT_MS:
        return 'INVALID', (f'resident test waited {waited} ms, under the '
                           f'{MIN_LEASE_WAIT_MS} ms floor: the lease was not contended')
    return 'KILLED', (f'resident test was refused inside the writer hold '
                      f'[{start_ms}, {end_ms}], waited {waited} ms and then passed'
                      f' (lock released at {released_ms} ms)')


def red(out):
    """Lease removed while the source guard holds the lock: the #1662 false FAIL."""
    with mutating_sources(root, [test_source, capture]):
        original = test_source.read_text()
        if original.count(LEASE_WAIT_ANCHOR) != 1:
            raise RuntimeError(f'#1662 stale lease anchor: '
                               f'{original.count(LEASE_WAIT_ANCHOR)} matches')
        test_source.write_text(
            original.replace(LEASE_WAIT_ANCHOR, LEASE_WAIT_REMOVED, 1))
        capture.write_text(capture.read_text() + CAPTURE_RESIDUE)
        result, _elapsed = run_cargo(out, 'lease-removed-false-fail',
                                     [RESIDENT_TEST, '--', '--exact'])
    return judge(result)


def stop_check(out):
    """Self-check the writer's exit contract against the real tree lock.

    Two runs, no cargo:

    * ``sigterm`` -- SIGTERM lands inside the window. A *requested* stop must
      restore the guarded bytes, record ``restored: true`` and exit 0; that is
      what lets ``stop_writer`` judge the exit status unconditionally.
    * ``own-failure`` -- the writer fails on its own after the hold
      (``PDRS_1662_WRITER_FAIL_AFTER_HOLD``). It must exit non-zero, and
      ``stop_writer`` must *report* it instead of returning cleanly (#1662
      review r8 P1).
    """
    _ensure_workspace_scanner()
    before = capture.read_bytes()
    rows = []
    for label, signal_it in (('sigterm', True), ('own-failure', False)):
        run_out = out / label
        run_out.mkdir(parents=True, exist_ok=False)
        environment = dict(os.environ)
        if not signal_it:
            environment['PDRS_1662_WRITER_FAIL_AFTER_HOLD'] = '1'
        process = subprocess.Popen(
            [sys.executable, '-I', str(Path(__file__).resolve()), '--writer',
             str(run_out), str(STOP_CHECK_HOLD_SECONDS)],
            stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True,
            start_new_session=True, env=environment)
        reported = None
        try:
            deadline = time.monotonic() + WRITER_ENTRY_BUDGET
            while not (run_out / 'writer-window-open').is_file():
                if process.poll() is not None:
                    break
                if time.monotonic() > deadline:
                    raise TimeoutError(f'#1662 {label}: writer never opened its window')
                time.sleep(0.05)
            if signal_it:
                # Exactly what ``stop_writer`` does once its wait times out.
                process.terminate()
                stdout, stderr = process.communicate(timeout=WRITER_EXIT_BUDGET)
                exclusive_write(run_out / 'stop-check.stdout', stdout or '')
                exclusive_write(run_out / 'stop-check.stderr', stderr or '')
            else:
                try:
                    stop_writer(process, run_out)
                except RuntimeError as error:
                    reported = str(error)
        finally:
            if process.poll() is None:
                _kill_group(process)
                _collect(process, 30)
        row = {'label': label, 'rc': process.returncode,
               'bytes_back': capture.read_bytes() == before,
               'released': read_released(run_out), 'reported': reported}
        rows.append(row)
        print(f'{label}: rc={row["rc"]} bytes_back={row["bytes_back"]} '
              f'released={row["released"]} reported={row["reported"]!r}', flush=True)
    sigterm, own_failure = rows
    failed = []
    if sigterm['rc'] != 0:
        failed.append(f'a requested stop exited {sigterm["rc"]}, not 0')
    if not sigterm['bytes_back']:
        failed.append('a requested stop left the guarded bytes mutated')
    if not (sigterm['released'] and sigterm['released'][1]):
        failed.append(f'a requested stop did not record restored: true '
                      f'(record={sigterm["released"]})')
    if own_failure['rc'] == 0:
        failed.append('a writer that failed on its own exited 0')
    if own_failure['reported'] is None:
        failed.append('stop_writer accepted a non-zero writer exit')
    return ('KILLED' if not failed else 'INVALID'), (
        '; '.join(failed) if failed else
        f'requested stop exited 0 and restored, own failure exited '
        f'{own_failure["rc"]} and was reported by stop_writer')


def main():
    arguments = sys.argv[1:]
    if arguments[:1] == ['--writer']:
        if len(arguments) != 3:
            raise SystemExit('#1662 writer usage: --writer OUT HOLD_SECONDS')
        return writer(Path(arguments[1]), float(arguments[2]))
    if arguments[:1] == ['--stop-check']:
        if len(arguments) != 2:
            raise SystemExit('#1662 stop-check usage: --stop-check NEW_OUTPUT_DIR')
        out = new_output_directory(arguments[1])
        status, reason = stop_check(out)
        print(f'writer exit contract: {status}: {reason}', flush=True)
        publish_summary(out, [{'name': 'writer-exit-contract',
                               'status': status, 'reason': reason}])
        return 0 if status == 'KILLED' else 1
    if len(arguments) != 1 or arguments[0].startswith('-'):
        raise SystemExit('usage: negative_controls.py NEW_OUTPUT_DIR')
    if not os.environ.get('CARGO_TARGET_DIR'):
        raise SystemExit('#1662 CARGO_TARGET_DIR must be supplied by the caller')
    out = new_output_directory(arguments[0])
    rows = []
    for name, phase in (('fixed-test-survives-writer-window', green),
                         ('lease-removed-false-fail', red)):
        status, reason = 'INVALID', 'not run'
        try:
            status, reason = phase(out)
        except Exception as error:
            status, reason = 'INVALID', f'{type(error).__name__}: {error}'
        rows.append({'name': name, 'status': status, 'reason': reason})
        print(f'{name}: {status}: {reason}', flush=True)
    publish_summary(out, rows)
    counts = {state: sum(row['status'] == state for row in rows)
              for state in ('KILLED', 'SURVIVED', 'INVALID')}
    print(f'#1662 {counts}', flush=True)
    return 0 if all(row['status'] == 'KILLED' for row in rows) else 1


if __name__ == '__main__':
    sys.exit(main())
