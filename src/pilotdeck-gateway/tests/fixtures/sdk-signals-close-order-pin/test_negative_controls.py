#!/usr/bin/env python3
# anchor-selfcheck: unsupported #1800 close-order pin abort-accounting meta-control on a temporary tree, no product mutation table
"""#1800: the close-order pin runner's hand-written abort accounting, pinned.

`sdk-signals-close-order-pin/negative_controls.py` is a self-managed loop (it
never calls ``negative_suite.run_cases``), so its ``except BaseException``
branch — revoke every recorded KILLED, record every unrun control as INVALID,
still publish the summary, exit 2 (#1508 / #1795) — is hand-written, and until
this file it had no in-repo test or meta-control: a regression would surface
only as a false PASS with green rows over an unverified tree.

Every exit of that runner is ``os._exit``, so it cannot be exec'd in-process
the way the issue-2495 meta-controls drive their scripts; each case here
instead spawns a real subprocess whose ``__file__`` points at an identical
path inside a temporary tree, so the runner's self-computed ``root``, catalog
and frozen ledgers all land in the temporary copy (the catalog file is a byte
copy of the real one, keeping every mutation anchor present exactly once).
The shared ``negative_suite.invalidate_completed`` is never mocked — half of
the pinned behavior is that the abort accounting routes through the shared
revocation — and only ``negative_process`` boundaries are patched in memory,
exactly like the issue-2495 / SessionRunnerTests precedents:

- healthy: the guard is a snapshot/restore stub and ``run_owned`` answers the
  check phases from a queue (positive passes, each control's negative fails at
  the dedicated #1382 marker plus its own criterion, the restored run
  passes). The selection must come out all KILLED at rc 0 — the positive
  control that makes every revoke assertion below non-vacuous;
- busy-lock: a real subprocess holds ``<tree>/.negative-source.lock`` through
  the real ``tree_lock``, the runner runs the real ``mutating_sources`` with
  its wait shortened 60 s -> 2 s (the issue-2495 GUARD_LOCK_FAST shape), and
  the lock timeout must land in the abort accounting: every selected row
  INVALID with ``run aborted: TimeoutError: source lock busy``, rc 2;
- mid-restore-fault: the runner's own restore read-back fails on the last
  control (its ``write_bytes`` restore is skipped, the mutant stays on disk
  and the ``#1382 restore failed`` check fires) — the already recorded row
  must be revoked with its prior diagnostic kept as a prefix, the unrun row
  recorded INVALID, the guard still restores the catalog, rc 2;
- guard-exit-fault: the stub guard raises after the body completed — the
  revoke-everything half of the same accounting (#979);
- cancel: ``run_owned`` raises KeyboardInterrupt during the last control's
  negative run — the deterministic shape of a Ctrl-C, which is a
  BaseException the per-control ``except Exception`` must not swallow.

The three-state exit contract is pinned at its other edge too: pre-guard
refusals (an unknown selection, a missing CARGO_TARGET_DIR) exit 1 with no
summary — those run the real, unmodified script directly, because nothing
needs patching before the guard. Run from this directory:
``python3 -I sdk-signals-close-order-pin/test_negative_controls.py``.
"""
import json
import os
from pathlib import Path
import shutil
import signal
import subprocess
import sys
import tempfile
import time
import unittest

FIXTURES = Path(__file__).resolve().parents[1]
PIN_SCRIPT = FIXTURES / 'sdk-signals-close-order-pin' / 'negative_controls.py'
CATALOG_SCRIPT = FIXTURES / 'sdk-signals' / 'negative_controls.py'

# The first per-ID row and the first boundary row of the registered table: one
# of each criterion family, in table order.
SELECTION = ('unordered-close-row-removed', 'unordered-close-boundary-dropped')
PIN_MARKER = '#1382 unordered-close rows must bind their own ID'
HEALTHY_REASON = 'preflight failed at its #1382 pin'

# The subprocess driver. It execs the real runner source with ``__file__``
# inside the temporary tree and patches only the negative_process boundaries
# the busy-lock case must keep real. Mode is the first argv element.
HARNESS_SOURCE = """\
import contextlib
import subprocess
import sys
from pathlib import Path

mode = sys.argv[1]
pin_path = Path(sys.argv[2])
real_fixtures = Path(sys.argv[3])
tree = Path(sys.argv[4]).resolve()
out = Path(sys.argv[5])
names = sys.argv[6:]

sys.path.insert(0, str(real_fixtures))
import negative_process

runner_path = tree / 'src/pilotdeck-gateway/tests/fixtures/sdk-signals-close-order-pin/negative_controls.py'
source = pin_path.read_text(encoding='utf-8')

if mode == 'busy-lock':
    # Real guard, real flock timeout: only the wait bound is shortened (the
    # issue-2495 GUARD_LOCK_FAST shape). The timeout fires before any git
    # work, so the temporary tree needs no repository.
    old = 'LOCK_TIMEOUT_SECONDS = 60'
    assert source.count(old) == 1, 'stale LOCK_TIMEOUT anchor'
    source = source.replace(old, 'LOCK_TIMEOUT_SECONDS = 2', 1)
else:
    catalog = tree / 'src/pilotdeck-gateway/tests/fixtures/sdk-signals/negative_controls.py'
    runner_globals = {'__file__': str(runner_path), '__name__': '__main__'}
    exit_fault = mode == 'guard-exit-fault'

    @contextlib.contextmanager
    def fake_guard(actual_root, requested, **_kwargs):
        snapshots = {Path(path): Path(path).read_bytes() for path in requested}
        try:
            yield snapshots
        finally:
            for path, data in snapshots.items():
                negative_process.write_source_atomic(actual_root, path, data)
            if exit_fault:
                raise RuntimeError('#1800 injected guard exit failure')

    calls = {'count': 0}

    def fake_run_owned(command, **_kwargs):
        # The check command carries no phase label, so the phases are served
        # from the runner's own deterministic order: positive-before, then
        # negative/restored per selected control (the SessionRunnerTests
        # stage-queue shape).
        index = calls['count']
        calls['count'] += 1
        if index == 0:
            return subprocess.CompletedProcess(
                command, 0, 'product anchors: PASS\\n', '')
        control = (index - 1) // 2
        if (index - 1) % 2 == 0:
            if mode == 'cancel' and control == len(names) - 1:
                raise KeyboardInterrupt()
            criterion = runner_globals['mutations'][control][4]
            return subprocess.CompletedProcess(
                command, 1, __PIN_MARKER__ + '\\n' + criterion + '\\n', '')
        return subprocess.CompletedProcess(
            command, 0, 'product anchors: PASS\\n', '')

    def fake_head_bytes(actual_root, relative, _baseline):
        return (Path(actual_root) / relative).read_bytes()

    negative_process.mutating_sources = fake_guard
    negative_process.run_owned = fake_run_owned
    negative_process._head_bytes = fake_head_bytes

    if mode == 'mid-restore-fault':
        # The runner's own restore check: skip the last control's write-back
        # so the mutant stays on disk and the read-back comparison raises.
        raw_write_bytes = Path.write_bytes
        restore_writes = {'count': 0}

        def skip_last_restore(self, data):
            if self == catalog:
                restore_writes['count'] += 1
                if restore_writes['count'] == len(names):
                    return len(data)
            return raw_write_bytes(self, data)

        Path.write_bytes = skip_last_restore
sys.argv = [str(runner_path), str(out), *names]
exec(compile(source, str(runner_path), 'exec'), runner_globals if mode != 'busy-lock' else {'__file__': str(runner_path), '__name__': '__main__'})
"""

# A real subprocess holds the temporary tree's source lock; the ready file is
# published atomically once the lock is held (the issue-2495 holder shape).
LOCK_HOLDER_SOURCE = (
    'import sys, time\n'
    'from pathlib import Path\n'
    'sys.path.insert(0, sys.argv[1])\n'
    'from negative_process import tree_lock\n'
    'with tree_lock(sys.argv[2], timeout=10):\n'
    '    Path(sys.argv[3] + ".tmp").write_text("held")\n'
    '    Path(sys.argv[3] + ".tmp").replace(sys.argv[3])\n'
    '    time.sleep(30)\n'
)

_BASES = []


class AbortAccountingTests(unittest.TestCase):
    """Every abort lands in the same accounting: all INVALID, summary, rc 2."""

    def run_pin(self, mode, names=SELECTION, base=None):
        """Drive the real runner source in a subprocess over a temporary tree.

        Returns ``(process, summary-or-None, tree, catalog_before)``; the
        catalog copy is taken before the run so every case can also prove the
        tree was restored whatever the abort did. ``base`` lets a caller reuse
        a tree it prepared (the busy-lock case must lock the very tree the
        runner then runs over).
        """
        if base is None:
            base = Path(tempfile.mkdtemp(prefix='pdrs-1800-pin-'))
            _BASES.append(base)
        tree = (base / 'tree').resolve()
        catalog = tree / 'src/pilotdeck-gateway/tests/fixtures/sdk-signals/negative_controls.py'
        catalog.parent.mkdir(parents=True)
        catalog.write_bytes(CATALOG_SCRIPT.read_bytes())
        for ledger in ('D1-SDK-WS-CLOSE-ORDER-001.md',
                       'D1-SDK-SIGNAL-UNHANDLED-REJECTION-001.md'):
            path = tree / 'specs/gateway-parity/diffs' / ledger
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text('#1800 temporary ledger placeholder\n', encoding='utf-8')
        harness = base / 'harness.py'
        # The dedicated #1382 marker is pinned in this module's constants, so
        # it reaches the subprocess as a literal rather than a second copy.
        harness.write_text(
            HARNESS_SOURCE.replace('__PIN_MARKER__', repr(PIN_MARKER)),
            encoding='utf-8')
        out = base / 'out'
        before = catalog.read_bytes()
        env = {**os.environ, 'CARGO_TARGET_DIR': str(base / 'unused-target')}
        process = subprocess.run(
            [sys.executable, '-I', str(harness), mode, str(PIN_SCRIPT),
             str(FIXTURES), str(tree), str(out), *names],
            capture_output=True, text=True, timeout=120, env=env)
        summary = None
        if (out / 'summary.json').is_file():
            summary = json.loads((out / 'summary.json').read_text(encoding='utf-8'))
        return process, summary, tree, before

    @staticmethod
    def counts_line(stdout):
        lines = [line for line in stdout.split('\n') if line.strip()]
        return json.loads(lines[-1]) if lines else None

    def assert_rows(self, summary, reasons):
        self.assertIsNotNone(summary, 'an aborting run must still publish its summary')
        self.assertEqual([row['mutation'] for row in summary['results']],
                         list(SELECTION),
                         '#1800 every selected control must stay accounted for')
        self.assertEqual([row['reason'] for row in summary['results']], reasons)
        self.assertEqual(summary['counts'],
                         {'KILLED': 0, 'SURVIVED': 0, 'INVALID': len(SELECTION)},
                         '#1800 an aborting run must revoke every kill and count '
                         'every row INVALID')

    def test_healthy_selection_publishes_killed_rows(self):
        process, summary, _tree, before = self.run_pin('healthy')
        self.assertEqual(process.returncode, 0,
                         f'a healthy selection must exit 0: {process.stderr!r}')
        self.assertIsNotNone(summary)
        self.assertEqual([row['status'] for row in summary['results']],
                         ['KILLED'] * len(SELECTION),
                         '#1800 the driving harness must really produce green '
                         'rows, or every revoke assertion below is vacuous')
        self.assertEqual([row['reason'] for row in summary['results']],
                         [HEALTHY_REASON] * len(SELECTION))
        self.assertEqual(summary['counts'],
                         {'KILLED': len(SELECTION), 'SURVIVED': 0, 'INVALID': 0})
        self.assertEqual(self.counts_line(process.stdout)['counts'],
                         {'KILLED': len(SELECTION), 'SURVIVED': 0, 'INVALID': 0})
        catalog = (_tree / 'src/pilotdeck-gateway/tests/fixtures/sdk-signals'
                   '/negative_controls.py')
        self.assertEqual(catalog.read_bytes(), before,
                         '#1800 a healthy run must restore the catalog byte for byte')

    def test_busy_source_lock_records_every_row_invalid(self):
        base = Path(tempfile.mkdtemp(prefix='pdrs-1800-lock-'))
        _BASES.append(base)
        tree = (base / 'tree').resolve()
        tree.mkdir(parents=True)
        ready = base / 'held'
        holder = subprocess.Popen(
            [sys.executable, '-I', '-c', LOCK_HOLDER_SOURCE,
             str(FIXTURES), str(tree), str(ready)],
            start_new_session=True)
        self.addCleanup(self._kill_holder, holder)
        deadline = time.monotonic() + 30
        while not ready.exists():
            self.assertIsNone(holder.poll(),
                             'the lock holder died before holding the tree lock')
            self.assertLess(time.monotonic(), deadline,
                            'the lock holder never published its ready file')
            time.sleep(0.05)
        process, summary, tree_back, before = self.run_pin('busy-lock', base=base)
        self.assertEqual(
            process.returncode, 2,
            f'#1800 a busy source lock must abort at rc 2, got '
            f'{process.returncode}: {process.stderr!r}{process.stdout!r}')
        lock = tree_back / '.negative-source.lock'
        # The busy-lock branch appends its holder hint to the abort reason
        # itself, so the summary carries it too.
        busy_reason = ('run aborted: TimeoutError: source lock busy: ' + str(lock)
                       + '; the other holder is most likely the #1145'
                         ' sdk-signals runner (it keeps .negative-source.lock'
                         ' for its whole case list) — rerun this pin after it'
                         ' finishes')
        self.assert_rows(summary, [busy_reason] * len(SELECTION))
        self.assertIn('#1382 run did not complete', process.stderr)
        # The busy-lock branch names the likely holder; a generic abort would
        # not produce this sentence.
        self.assertIn('most likely the #1145 sdk-signals runner', process.stderr)
        self.assertEqual(self.counts_line(process.stdout)['counts']['INVALID'],
                         len(SELECTION))
        catalog = (tree_back / 'src/pilotdeck-gateway/tests/fixtures/sdk-signals'
                   '/negative_controls.py')
        self.assertEqual(catalog.read_bytes(), before,
                         '#1800 an entry-phase abort never wrote the catalog')

    @staticmethod
    def _kill_holder(holder):
        # Group kill, not a bare kill(): the holder runs in its own session
        # (the issue-2495 cleanup shape). A vanished group is not an error.
        if holder.poll() is None:
            try:
                os.killpg(os.getpgid(holder.pid), signal.SIGKILL)
            except ProcessLookupError:
                pass
        holder.wait(timeout=30)

    def test_mid_run_restore_failure_revokes_and_completes_accounting(self):
        # The #1800 core: a restore failure after a green row must revoke the
        # kill (keeping its diagnostic as the prefix, the #979 shape) and
        # still record the unrun control — never a green row over an
        # unverified tree.
        process, summary, tree, before = self.run_pin('mid-restore-fault')
        self.assertEqual(process.returncode, 2,
                         f'#1800 a mid-run restore failure must exit 2: '
                         f'{process.stderr!r}{process.stdout!r}')
        failure = 'run aborted: RuntimeError: #1382 restore failed: ' \
                  'src/pilotdeck-gateway/tests/fixtures/sdk-signals/negative_controls.py'
        self.assert_rows(summary, [HEALTHY_REASON + '; ' + failure, failure])
        catalog = (tree / 'src/pilotdeck-gateway/tests/fixtures/sdk-signals'
                   '/negative_controls.py')
        self.assertEqual(catalog.read_bytes(), before,
                         '#1800 the guard must still restore the tree its '
                         'runner failed to')

    def test_guard_exit_failure_revokes_every_completed_row(self):
        process, summary, tree, before = self.run_pin('guard-exit-fault')
        self.assertEqual(process.returncode, 2,
                         f'#1800 a guard exit failure must exit 2: '
                         f'{process.stderr!r}{process.stdout!r}')
        failure = 'run aborted: RuntimeError: #1800 injected guard exit failure'
        self.assert_rows(summary, [HEALTHY_REASON + '; ' + failure,
                                   HEALTHY_REASON + '; ' + failure])
        self.assertIn('#1382 run did not complete', process.stderr)

    def test_cancellation_voids_recorded_kills_and_unrun_rows(self):
        # KeyboardInterrupt is a BaseException the per-control ``except
        # Exception`` must not swallow: the accounting sees it as a
        # cancellation of the whole run.
        process, summary, tree, before = self.run_pin('cancel')
        self.assertEqual(process.returncode, 2,
                         f'#1800 a cancellation must exit 2: '
                         f'{process.stderr!r}{process.stdout!r}')
        self.assert_rows(summary,
                         [HEALTHY_REASON + '; run aborted: KeyboardInterrupt: ',
                          'run aborted: KeyboardInterrupt: '])
        catalog = (tree / 'src/pilotdeck-gateway/tests/fixtures/sdk-signals'
                   '/negative_controls.py')
        self.assertEqual(catalog.read_bytes(), before,
                         '#1800 a cancelled run must still restore the tree')


class PreGuardRefusalTests(unittest.TestCase):
    """The three-state contract's other edge: pre-guard refusals exit 1."""

    def refusal(self, argv, env):
        base = Path(tempfile.mkdtemp(prefix='pdrs-1800-refusal-'))
        _BASES.append(base)
        out = base / 'out'
        result = subprocess.run(
            [sys.executable, '-I', str(PIN_SCRIPT), str(out), *argv],
            capture_output=True, text=True, timeout=60, env=env)
        return result, out

    def test_unknown_selection_exits_one_without_a_summary(self):
        result, out = self.refusal(
            ['no-such-control-1800'],
            {**os.environ, 'CARGO_TARGET_DIR': '/nonexistent-cargo-target'})
        self.assertEqual(result.returncode, 1,
                         f'#1800 an unknown selection is a pre-guard refusal '
                         f'(rc 1): {result.stderr!r}')
        self.assertIn('#1382 unknown or duplicate selection', result.stderr)
        self.assertFalse(out.exists(),
                         '#1800 a pre-guard refusal must not create evidence')

    def test_missing_cargo_target_exits_one_without_a_summary(self):
        result, out = self.refusal(
            [SELECTION[0]],
            {key: value for key, value in os.environ.items()
             if key != 'CARGO_TARGET_DIR'})
        self.assertEqual(result.returncode, 1,
                         f'#1800 a missing CARGO_TARGET_DIR is a pre-guard '
                         f'refusal (rc 1): {result.stderr!r}')
        self.assertIn('#1382 CARGO_TARGET_DIR must be supplied', result.stderr)
        self.assertFalse(out.exists())


def tearDownModule():
    for base in _BASES:
        shutil.rmtree(base, ignore_errors=True)


if __name__ == '__main__':
    unittest.main()
