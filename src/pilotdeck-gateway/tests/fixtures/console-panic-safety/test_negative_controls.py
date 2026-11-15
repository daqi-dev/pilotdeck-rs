#!/usr/bin/env python3
# anchor-selfcheck: unsupported guard tests for the console-panic-safety negative controls, exec modified source only
"""#3152 fix-r2 (review boundary#3, with sibling#2 ≡ boundary#1, sibling#3
and concurrency#1 ≡ boundary#2): the console-panic-safety control's repair
verdict paths.

fix-r1 pinned only the "repair build returns nonzero" leg of the
shared-lexer repair. Three legs had no driver: a *raised* slot-wait or
build error (``TimeoutExpired`` / ``OSError``) used to escape the
per-mutant ``finally`` and skip ``summary.json`` entirely, the
``failures`` list used to reach only stderr (never the exit code), and
the final line's numerator and denominator used to span different
domains (``summary`` vs ``run_rows``), printing the byte-identical green
line beside a failed exit. Each case execs the real script with the
cargo surface staged — ``mutating_sources`` / ``write_source_atomic``
never touch the tree, ``run_owned`` / ``wait_for_slot`` return staged
results — so nothing needs cargo, a tree lock or a committed tree; the
shared-lexer mutant (the one row whose repair leg runs) is the scenario
vehicle. Degraded-source cases re-exec the script with one source-level
edit injected, the same shape an accidental edit would take
(the settings-gateway driver's shape).

Run from this directory: ``python3 -m unittest console-panic-safety/test_negative_controls.py``.
"""
import contextlib
import io
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import types
import unittest
from contextlib import redirect_stderr, redirect_stdout
from unittest.mock import patch

FIXTURES = Path(__file__).resolve().parents[1]
SCRIPT = FIXTURES / 'console-panic-safety' / 'negative_controls.py'
sys.path.insert(0, str(FIXTURES))
import negative_process  # noqa: E402 — the fixtures tree, like the script's own import

# The one mutant whose repair leg runs: the shared-lexer row, bound to the
# scanner fixture. The staged logs below satisfy the real shared judges
# (``baseline_passed`` / ``named_test_failed`` / ``panic_evidence``) — the
# marker is the row's own CHECKS fragment.
MUTANT = 'shared-lexer-rpath-kept'
SCANNER_TEST = 'anchor_scanner_resolves_rustc_module_semantics'
MARKER = '#3152 fix-r12 rustc E11: the r# normalization regressed'
GREEN_LINE = '1/1 killed; every source restored byte-for-byte'
FAILED_REPAIR_LINE = '1/2 killed; 1 verdict row(s) not killed'

GREEN_LOG = (
    'running 1 test\n'
    f'test {SCANNER_TEST} ... ok\n'
    '\n'
    'test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; '
    'filtered out; finished in 0.01s\n'
)
KILLED_LOG = (
    'running 1 test\n'
    f'test {SCANNER_TEST} ... FAILED\n'
    '\n'
    f"thread '{SCANNER_TEST}' panicked at "
    'src/pilotdeck-gateway/tests/console_panic_safety.rs:1234:5:\n'
    f'{MARKER}\n'
    'note: run with `RUST_BACKTRACE=1` environment variable to display '
    'a backtrace\n'
    'test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; '
    'filtered out; finished in 0.01s\n'
    '\n'
    'error: test failed\n'
)

# One source-level degradation each, bound to the green test named in the
# case's docstring (review boundary#3's driver shape).
NARROWED_EXCEPT = (
    'except (subprocess.SubprocessError, OSError) as error:',
    'except OSError as error:')
RUN_ROWS_DENOMINATOR = (
    'if killed_rows == len(summary):',
    'if killed_rows == len(run_rows):')
REMOVE_REPAIR_ROW_WRITE = (
    "                    summary.append(dict(\n"
    "                        name=f'{name}-repair', test=test, status='INVALID',\n"
    "                        reason=f'repair build failed: '\n"
    "                               f'{type(error).__name__}: {error}',\n"
    "                        sourceSha256=before))\n",
    '')
REMOVE_FAILURES_GUARD = ('if failures:\n    sys.exit(1)\n', '')


def _staged_result(returncode, output):
    return types.SimpleNamespace(returncode=returncode, stdout=output, stderr='')


class StagedCargo:
    """The whole cargo surface, staged: slot waits, test runs, repair build.

    The script's calls arrive in a fixed order for the one selected mutant
    (baseline slot + run, mutant slot + run, repair slot + build); the
    third slot wait is the repair one, so ``repair_slot_timeout`` stages
    the raised-error leg exactly where the fix-r2 ``except`` has to catch
    it.
    """

    def __init__(self, *, repair=None, repair_slot_timeout=False):
        self.repair = repair if repair is not None else _staged_result(0, 'repair ok\n')
        self.repair_slot_timeout = repair_slot_timeout
        self.slot_waits = 0
        self.test_runs = 0

    def wait_for_slot(self, script, **_kwargs):
        self.slot_waits += 1
        if self.repair_slot_timeout and self.slot_waits == 3:
            raise subprocess.TimeoutExpired(cmd=str(script), timeout=600)
        return _staged_result(0, '')

    def run_owned(self, command, **_kwargs):
        if command[1] == 'build':
            return self.repair
        self.test_runs += 1
        if self.test_runs == 1:  # the unmutated baseline must be green
            return _staged_result(0, GREEN_LOG)
        return _staged_result(101, KILLED_LOG)


@contextlib.contextmanager
def _staged_mutating_sources(root, paths, **_kwargs):
    """Snapshot without the tree: nothing is written, so nothing is restored."""
    yield {Path(path): Path(path).read_bytes() for path in paths}


def _staged_write(_root, _path, _data, *, mode=None):
    """No source is ever published — the driver must not touch the tree."""


def run(*, edits=(), repair=None, repair_slot_timeout=False):
    """Exec the control script with the cargo surface staged; return a record.

    ``edits`` is a sequence of ``(before, after)`` pairs applied to the real
    script text (each must occur exactly once) — the degraded-source cases
    use them to inject the mutation under test, the scenarios below to
    simulate the future apparatus failure the ``failures`` guard exists for.
    """
    source = SCRIPT.read_text(encoding='utf-8')
    for before, after in edits:
        if source.count(before) != 1:
            raise AssertionError(
                f'test anchor occurs {source.count(before)} times: {before!r}')
        source = source.replace(before, after, 1)
    base = Path(tempfile.mkdtemp(prefix='console-panic-safety-3152-'))
    out = base / 'out'
    cargo = StagedCargo(repair=repair, repair_slot_timeout=repair_slot_timeout)
    environment = {key: value for key, value in os.environ.items()
                   if key != 'PDRS_NEGATIVE_BASELINE'}
    environment['CARGO_TARGET_DIR'] = str(base / 'target')
    record = types.SimpleNamespace(code=0, raised=None, out=out, base=base)
    stdout, stderr = io.StringIO(), io.StringIO()
    with patch.dict(os.environ, environment, clear=True), \
            patch.object(sys, 'argv',
                         [str(SCRIPT), str(out), MUTANT]), \
            patch.object(negative_process, 'mutating_sources',
                         _staged_mutating_sources), \
            patch.object(negative_process, 'write_source_atomic',
                         _staged_write), \
            patch.object(negative_process, 'wait_for_slot',
                         cargo.wait_for_slot), \
            patch.object(negative_process, 'run_owned', cargo.run_owned), \
            redirect_stdout(stdout), redirect_stderr(stderr):
        try:
            exec(compile(source, str(SCRIPT), 'exec'),
                 {'__name__': '__main__', '__file__': str(SCRIPT)})
        except SystemExit as error:
            record.code = error.code if isinstance(error.code, int) else 1
        except Exception as error:  # a degraded script must not kill the driver
            record.raised = f'{type(error).__name__}: {error}'
            record.code = 1
    record.stdout = stdout.getvalue()
    record.stderr = stderr.getvalue()
    record.summary = (json.loads((out / 'summary.json').read_text())
                      if (out / 'summary.json').is_file() else None)
    return record


class RepairVerdictPaths(unittest.TestCase):
    """The repair leg's failure shapes and the run's final line (fix-r2)."""

    def test_green_round_prints_the_unchanged_green_line(self):
        record = run()
        self.addCleanup(shutil.rmtree, record.base, True)
        self.assertIsNone(record.raised)
        self.assertEqual(record.code, 0)
        self.assertEqual(record.stdout.splitlines()[-1], GREEN_LINE)
        self.assertEqual([row['status'] for row in record.summary], ['KILLED'])
        self.assertNotIn('not killed', record.stderr)

    def test_failed_repair_build_is_its_own_invalid_row(self):
        record = run(repair=_staged_result(1, 'error: could not compile\n'))
        self.addCleanup(shutil.rmtree, record.base, True)
        self.assertIsNone(record.raised)
        self.assertEqual(record.code, 1)
        self.assertEqual([row['status'] for row in record.summary],
                         ['KILLED', 'INVALID'])
        self.assertEqual(record.summary[1]['name'], f'{MUTANT}-repair')
        self.assertEqual(record.summary[1]['returncode'], 1)
        # fix-r2 review concurrency#1 ≡ boundary#2: same-domain final line.
        self.assertEqual(record.stdout.splitlines()[-1], FAILED_REPAIR_LINE)
        self.assertIn('not killed', record.stderr)

    def test_raised_repair_error_is_its_own_invalid_row(self):
        # fix-r2 review sibling#2 ≡ boundary#1: the raised leg (the third
        # slot wait is the repair one) must not escape as a traceback.
        record = run(repair_slot_timeout=True)
        self.addCleanup(shutil.rmtree, record.base, True)
        self.assertIsNone(record.raised)
        self.assertEqual(record.code, 1)
        self.assertIsNotNone(record.summary,
                             'summary.json must still be written')
        self.assertEqual([row['status'] for row in record.summary],
                         ['KILLED', 'INVALID'])
        self.assertEqual(record.summary[1]['name'], f'{MUTANT}-repair')
        self.assertIn('TimeoutExpired', record.summary[1]['reason'])
        self.assertEqual(record.stdout.splitlines()[-1], FAILED_REPAIR_LINE)
        self.assertIn('not killed', record.stderr)

    def test_failures_only_apparatus_failure_still_exits_nonzero(self):
        # fix-r2 review sibling#3 (the turn-submit-panic-close class close):
        # every ``failures`` write today lands transitively in a non-KILLED
        # summary row, so the guard is pinned through the future failure it
        # exists for — the repair exception path degraded to a failures-only
        # write (its summary row removed). The guard must still fail the run.
        record = run(edits=[REMOVE_REPAIR_ROW_WRITE], repair_slot_timeout=True)
        self.addCleanup(shutil.rmtree, record.base, True)
        self.assertIsNone(record.raised)
        self.assertEqual(record.code, 1,
                         'a failures-only apparatus failure must exit nonzero')
        self.assertEqual([row['status'] for row in record.summary], ['KILLED'])


class DegradedScriptNegativeControls(unittest.TestCase):
    """One source-level degradation each; the bound green test must go red."""

    def test_narrowed_except_lets_the_repair_timeout_escape(self):
        # Bound to RepairVerdictPaths.test_raised_repair_error_is_its_own_
        # invalid_row. With the except narrowed to OSError, the repair
        # TimeoutExpired escapes the finally as a traceback: summary.json is
        # never written and the exit code is the traceback's, not the
        # verdict's (fix-r2 review sibling#2 ≡ boundary#1 — the degraded
        # script is exactly the pre-fix-r2 shape).
        record = run(edits=[NARROWED_EXCEPT], repair_slot_timeout=True)
        self.addCleanup(shutil.rmtree, record.base, True)
        self.assertIsNotNone(
            record.raised,
            'the narrowed except must let the TimeoutExpired escape; '
            'RepairVerdictPaths.test_raised_repair_error_is_its_own_invalid_row '
            'would still pass on this degraded script')
        self.assertIn('TimeoutExpired', record.raised)
        self.assertIsNone(record.summary,
                           'the escaped timeout must skip summary.json entirely')

    def test_run_rows_denominator_prints_the_green_wording(self):
        # Bound to RepairVerdictPaths.test_failed_repair_build_is_its_own_
        # invalid_row. With the final-line condition back on len(run_rows),
        # the repair-INVALID run's last line regains the green wording
        # beside its exit 1 (fix-r2 review concurrency#1 ≡ boundary#2 — the
        # degraded script is exactly the pre-fix-r2 shape).
        record = run(edits=[RUN_ROWS_DENOMINATOR],
                     repair=_staged_result(1, 'error: could not compile\n'))
        self.addCleanup(shutil.rmtree, record.base, True)
        self.assertEqual(record.code, 1)
        self.assertEqual(record.summary[-1]['status'], 'INVALID')
        self.assertIn('restored byte-for-byte', record.stdout.splitlines()[-1],
                      'the degraded denominator must print the green wording; '
                      'RepairVerdictPaths.test_failed_repair_build_is_its_own_'
                      'invalid_row would still pass on this degraded script')

    def test_removed_failures_guard_exits_zero_on_a_failures_only_run(self):
        # Bound to RepairVerdictPaths.test_failures_only_apparatus_failure_
        # still_exits_nonzero. With both the row write and the guard gone,
        # the simulated future apparatus failure exits 0 with an all-KILLED
        # verdict — the false green the guard exists to close (fix-r2 review
        # sibling#3; the row-write degradation is the scenario's own, the
        # guard's removal is the mutation under test).
        record = run(edits=[REMOVE_REPAIR_ROW_WRITE, REMOVE_FAILURES_GUARD],
                     repair_slot_timeout=True)
        self.addCleanup(shutil.rmtree, record.base, True)
        self.assertEqual(record.code, 0,
                         'without the guard a failures-only apparatus failure '
                         'exits 0; RepairVerdictPaths.'
                         'test_failures_only_apparatus_failure_still_exits_nonzero '
                         'would fail on this degraded script')
        self.assertEqual([row['status'] for row in record.summary], ['KILLED'])
        self.assertIn('not killed', record.stderr)


if __name__ == '__main__':
    unittest.main()
