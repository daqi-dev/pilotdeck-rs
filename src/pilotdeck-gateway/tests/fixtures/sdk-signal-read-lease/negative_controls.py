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
WRITER_ENTRY_BUDGET = 960
CARGO_TIMEOUT = 1800


def cargo_argv(suffix):
    return [sys.executable, '-I', '-B', str(wrapper), 'cargo', 'test', '--offline',
            '--manifest-path', 'src/Cargo.toml', '-p', 'pilotdeck-cli',
            '--test', 'sdk_signal_exit', *suffix]


def run_cargo(out, label, suffix):
    slot = wait_for_slot(root / 'scripts/cargo-slots.sh', timeout=900, interval=15)
    exclusive_write(out / f'{label}.slots.log', slot.stdout + slot.stderr)
    result = run_owned(cargo_argv(suffix), cwd=root, env=os.environ.copy(),
                       timeout=CARGO_TIMEOUT)
    exclusive_write(out / f'{label}.stdout', result.stdout)
    exclusive_write(out / f'{label}.stderr', result.stderr)
    return result


def writer(out, window):
    """Independent committed-mode writer: hold the tree lock over one mutant."""
    with mutating_sources(root, [capture]):
        capture.write_text(capture.read_text() + CAPTURE_RESIDUE)
        # Publish the window atomically: a reader that sees the file must also
        # see the mutant bytes (#166 tmp-and-rename).
        temporary = out / 'writer-window-open.tmp'
        temporary.write_text('open\n')
        temporary.replace(out / 'writer-window-open')
        time.sleep(window)
    return 0


def stop_writer(process, out):
    try:
        stdout, stderr = process.communicate(timeout=WRITER_ENTRY_BUDGET)
    except subprocess.TimeoutExpired:
        os.killpg(process.pid, signal.SIGKILL)
        stdout, stderr = process.communicate(timeout=30)
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
    """Fixed test under the writer protocol: wait for the lease, read restored bytes."""
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
        result = run_cargo(out, 'fixed-test-survives-writer-window',
                           [RESIDENT_TEST, '--', '--exact'])
    finally:
        stop_writer(process, out)
    if passed(result, RESIDENT_TEST):
        return 'KILLED', 'resident test waited out the writer and passed'
    status, reason = judge(result)
    return status, 'fixed test did not survive the writer window: ' + reason


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
        result = run_cargo(out, 'lease-removed-false-fail',
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
