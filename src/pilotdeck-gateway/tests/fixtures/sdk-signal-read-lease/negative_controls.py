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
CARGO_TIMEOUT = 1800


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
    """
    armed = [True]

    def stop(_signum, _frame):
        if armed[0]:
            armed[0] = False
            raise SystemExit('#1662 writer stopped before its window closed')

    signal.signal(signal.SIGTERM, stop)
    with mutating_sources(root, [capture], lock_timeout=WRITER_LOCK_TIMEOUT):
        try:
            capture.write_text(capture.read_text() + CAPTURE_RESIDUE)
            # Publish the window atomically: a reader that sees the file must also
            # see the mutant bytes (#166 tmp-and-rename).
            opened = epoch_ms()
            publish_hold(out, opened, None)
            publish(out / 'writer-window-open', 'open\n')
            time.sleep(hold)
            # Recorded while the lock is still held: the reader can only acquire
            # it after this instant (plus the restore, covered by the tolerance).
            publish_hold(out, opened, epoch_ms())
        finally:
            # Disarm on every path out of the window, including an exception:
            # restoration must never run with the handler still armed.
            armed[0] = False
    return 0


def force_stop(process, out):
    """Recover when ``stop_writer`` itself was interrupted.

    Graceful first (the writer's SIGTERM handler unwinds ``mutating_sources``,
    which restores every guarded byte), then a bounded wait, and only then a
    kill -- a kill during restoration can leave a mutant or a truncated file on
    disk (#1662 review r5 P0). Returns True when the writer exited cleanly.
    """
    if process.poll() is not None:
        return process.returncode == 0
    try:
        process.terminate()
        process.communicate(timeout=WRITER_EXIT_BUDGET)
        return process.returncode == 0
    except Exception:
        pass
    _kill_group(process)
    _collect(process, 30)
    return False


def stop_writer(process, out):
    """Bounded stop: let the writer restore first, then terminate, then kill."""
    try:
        stdout, stderr = process.communicate(timeout=WRITER_EXIT_BUDGET)
    except subprocess.TimeoutExpired:
        # Graceful first: the handler unwinds mutating_sources and restores.
        process.terminate()
        try:
            stdout, stderr = process.communicate(timeout=WRITER_EXIT_BUDGET)
        except subprocess.TimeoutExpired:
            _kill_group(process)
            stdout, stderr = _collect(process, 30)
    exclusive_write(out / 'writer.stdout', stdout or '')
    exclusive_write(out / 'writer.stderr', stderr or '')
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
            # The stop itself can be interrupted (SIGINT during communicate).
            # Recover gracefully and only kill as a last resort; ``restored``
            # records whether we can still claim the sources were restored.
            restored = force_stop(process, out)
            if not restored:
                stop_error = RuntimeError(
                    f'{type(error).__name__}: {error}; writer had to be killed, '
                    f'source restoration is NOT confirmed')
    for interrupt in (failure, stop_error):
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
    # holder (#1662 review r4/r5 P0).
    waited, first, blocked, at = max(waits, key=lambda row: row[0])
    if blocked == 0:
        return 'INVALID', ('the resident test never observed a refused attempt, so it never '
                           'met an exclusive holder; this row is no evidence for the lease')
    if not (start_ms <= blocked <= end_ms):
        return 'INVALID', (f'resident test was refused at {blocked} ms, outside the writer '
                           f'hold [{start_ms}, {end_ms}]: that refusal was not this writer')
    if not first <= blocked <= at:
        return 'INVALID', (f'inconsistent marker: first={first} blocked={blocked} at={at}')
    if waited < MIN_LEASE_WAIT_MS:
        return 'INVALID', (f'resident test waited {waited} ms, under the '
                           f'{MIN_LEASE_WAIT_MS} ms floor: the lease was not contended')
    return 'KILLED', (f'resident test was refused inside the writer hold '
                      f'[{start_ms}, {end_ms}], waited {waited} ms and then passed')


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


def main():
    arguments = sys.argv[1:]
    if arguments[:1] == ['--writer']:
        if len(arguments) != 3:
            raise SystemExit('#1662 writer usage: --writer OUT HOLD_SECONDS')
        return writer(Path(arguments[1]), float(arguments[2]))
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
