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
mutant being killed; in GREEN only a *pass* counts, and the pass must have cost
at least ``CONTENTION_FLOOR`` seconds inside cargo -- otherwise the resident
test never met the writer and proves nothing. GREEN failures of any kind are
INVALID, never KILLED.

Run only after commit (mutating_sources refuses a dirty tree):
    CARGO_TARGET_DIR=<target> python3 -I .../sdk-signal-read-lease/negative_controls.py NEW_OUTPUT_DIR
"""
import os
import signal
import subprocess
import sys
import time
from pathlib import Path

sys.dont_write_bytecode = True
sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from negative_process import (
    _collect,
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
WINDOW_SECONDS = 60
# The writer fails fast instead of queueing behind a long committed suite: it
# only has to win the lock once, and a slow acquisition is a busy workspace,
# not evidence about the lease. 300 s stays well inside the 900 s default other
# devices use, so this control never becomes the longest lock holder.
WRITER_LOCK_TIMEOUT = 300
# Writer start -> lock acquisition (<= WRITER_LOCK_TIMEOUT) -> window open.
WRITER_ENTRY_BUDGET = WRITER_LOCK_TIMEOUT + 300
# Writer finish after the reader returns: the rest of the window plus restore.
WRITER_EXIT_BUDGET = WINDOW_SECONDS + 120
# A prewarmed `cargo test --exact` binary starts in ~1-2 s. A resident test
# that really waited on the lease cannot finish before the writer released it,
# which is WINDOW_SECONDS after the window opened, so any pass faster than this
# floor never contended and is not evidence for the fix (#1662 review P0).
CONTENTION_FLOOR = 5.0
CARGO_TIMEOUT = 1800


def cargo_argv(suffix):
    return [sys.executable, '-I', '-B', str(wrapper), 'cargo', 'test', '--offline',
            '--manifest-path', 'src/Cargo.toml', '-p', 'pilotdeck-cli',
            '--test', 'sdk_signal_exit', *suffix]


def run_cargo(out, label, suffix):
    """Run one cargo invocation; return ``(result, seconds spent inside it)``.

    The elapsed time is measured around ``run_owned`` only: waiting for a cargo
    slot is a scheduling delay, not lease contention, and must not be counted
    as evidence that the resident test blocked on the writer.
    """
    slot = wait_for_slot(root / 'scripts/cargo-slots.sh', timeout=900, interval=15)
    exclusive_write(out / f'{label}.slots.log', slot.stdout + slot.stderr)
    started = time.monotonic()
    result = run_owned(cargo_argv(suffix), cwd=root, env=os.environ.copy(),
                       timeout=CARGO_TIMEOUT)
    elapsed = time.monotonic() - started
    exclusive_write(out / f'{label}.stdout', result.stdout)
    exclusive_write(out / f'{label}.stderr', result.stderr)
    return result, elapsed


def writer(out, window):
    """Independent committed-mode writer: hold the tree lock over one mutant.

    SIGTERM unwinds through ``mutating_sources`` so every guarded byte is
    restored before the process exits. A SIGKILL leaves the mutant on disk;
    the next ``mutating_sources`` run then refuses the dirty tree instead of
    silently mutating a mutated baseline (#1662 review P1).
    """
    def stop(_signum, _frame):
        raise SystemExit('#1662 writer stopped before its window closed')

    signal.signal(signal.SIGTERM, stop)
    with mutating_sources(root, [capture], lock_timeout=WRITER_LOCK_TIMEOUT):
        capture.write_text(capture.read_text() + CAPTURE_RESIDUE)
        # Publish the window atomically: a reader that sees the file must also
        # see the mutant bytes (#166 tmp-and-rename).
        temporary = out / 'writer-window-open.tmp'
        temporary.write_text('open\n')
        temporary.replace(out / 'writer-window-open')
        time.sleep(window)
    return 0


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


def green(out):
    """Fixed test under the writer protocol: wait for the lease, read restored bytes.

    GREEN has its own verdicts on purpose. ``judge()`` is the RED direction: a
    golden-sha failure there means the mutant killed the test. Here the same
    failure means the fix stopped working, so reusing ``judge()`` would report
    a broken lease as a passing control (#1662 review P0). Only a pass counts.
    """
    run_cargo(out, 'prewarm-compile', ['--no-run'])
    process = subprocess.Popen(
        [sys.executable, '-I', str(Path(__file__).resolve()), '--writer', str(out),
         str(WINDOW_SECONDS)],
        stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True,
        start_new_session=True)
    try:
        deadline = time.monotonic() + WRITER_ENTRY_BUDGET
        while not (out / 'writer-window-open').is_file():
            if process.poll() is not None:
                raise RuntimeError('#1662 writer exited before opening its window')
            if time.monotonic() > deadline:
                raise TimeoutError('#1662 writer never opened its mutation window')
            time.sleep(0.5)
        result, elapsed = run_cargo(out, 'fixed-test-survives-writer-window',
                                    [RESIDENT_TEST, '--', '--exact'])
    finally:
        stop_writer(process, out)
    if not passed(result, RESIDENT_TEST):
        log = result.stdout + result.stderr
        return 'INVALID', (
            f'fixed test did not survive the writer window (rc={result.returncode}, '
            f'golden-sha failure={SHA_MARKER in panic_evidence(log, RESIDENT_TEST)}); '
            f'GREEN only accepts the lease wait followed by a pass')
    if elapsed < CONTENTION_FLOOR:
        return 'INVALID', (
            f'resident test passed in {elapsed:.1f}s, under the {CONTENTION_FLOOR:.0f}s '
            f'contention floor: it never waited on the writer lease, so this row is '
            f'no evidence for the fix (rerun when cargo slots are free)')
    return 'KILLED', f'resident test waited {elapsed:.1f}s on the writer and passed'


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
            raise SystemExit('#1662 writer usage: --writer OUT WINDOW_SECONDS')
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
