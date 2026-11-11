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
  BaseException the per-control ``except Exception`` must not swallow;
- survived: ``run_owned`` answers every phase green — the verdict edge none
  of the abort modes can reach: the mutated catalog's preflight still
  passes, so every row is SURVIVED and the run exits 1, the code the
  contract reserves for "the device ran and a control survived". Routing
  that path to rc 0 (a coordinator reads a red device as green) or to rc 2
  (it reads "the device never ran") must both land here as a red test;
- invalid-verdict: the negative run fails *without* the dedicated #1382
  marker — the shape a stale anchor produces on real hardware — so the row
  is INVALID through the verdict path, not the abort path. Every INVALID
  row the abort modes above produce comes from the ``aborted`` branch, and
  the runner checks that branch *before* ``if invalid: os._exit(2)``, so
  the two exit-2 routes are different code: routing the verdict one to
  rc 0 (a coordinator reads "the device never produced a verdict" as all
  green) or to rc 1 (it reads it as "a control survived") must both land
  here as a red test;
- summary-publish-fault: a healthy all-KILLED run whose summary rename
  fails — no summary may survive a failed publish (a stale or half-written
  one would read as evidence), and rc 2 must say the device produced no
  verdict.
- stdout-write-fault: a healthy all-KILLED run whose final counts line
  cannot be written — the write fails with the measured /dev/full shape
  (OSError ENOSPC, not a pipe error), so it must not be swallowed as
  ``... | head``: rc 2 with a stderr diagnostic naming the failure, while
  the summary on disk still carries the completed verdict rows.

Both senses of exit 1 are pinned: the verdict path's 1 above, and the
pre-guard refusals' 1 (an unknown selection, a missing CARGO_TARGET_DIR)
with no summary — those run the real, unmodified script directly, because
nothing needs patching before the guard. Run from this directory:
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
        if mode == 'survived':
            # The verdict edge no abort mode can reach: every phase green, so
            # the negative run's preflight still passes and the row survives.
            return subprocess.CompletedProcess(
                command, 0, 'product anchors: PASS\\n', '')
        if index == 0:
            return subprocess.CompletedProcess(
                command, 0, 'product anchors: PASS\\n', '')
        control = (index - 1) // 2
        if (index - 1) % 2 == 0:
            if mode == 'cancel' and control == len(names) - 1:
                raise KeyboardInterrupt()
            if mode == 'invalid-verdict':
                # The verdict path's INVALID row: the negative run fails
                # without any dedicated #1382 marker (the stale-anchor shape),
                # so the runner's else branch records INVALID and the exit
                # must come from the verdict path, never the abort branch.
                return subprocess.CompletedProcess(
                    command, 1, 'preflight failed elsewhere\\n', '')
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

    if mode == 'summary-publish-fault':
        # The publish step itself: the temp summary file renames onto
        # summary.json, so fail exactly that rename after an otherwise
        # healthy run. write_source_atomic (the guard's restore) goes
        # through os.replace and is untouched.
        raw_replace = Path.replace

        def fail_summary_publish(self, target):
            if self.name == 'summary.json.tmp':
                raise OSError('#1800 injected summary publish failure')
            return raw_replace(self, target)

        Path.replace = fail_summary_publish

    if mode == 'stdout-write-fault':
        # #1800 fix-r3 (comment follow-up 1): the final counts line fails
        # with the measured /dev/full shape — OSError(ENOSPC), never a pipe
        # error (the review's premise, reproduced by this file's host
        # evidence). Only that one line fails: the per-control record lines
        # still reach the reader, so the run itself completed and only the
        # stdout delivery broke. The swap is in-process (nothing inherited
        # from the environment is consulted) and stderr keeps its real
        # stream, so the diagnostic the fix must print stays observable.
        real_stdout = sys.stdout

        class FullDiskStdout:
            def write(self, text):
                if '"counts"' in text:
                    raise OSError(28, 'No space left on device')
                return real_stdout.write(text)

            def flush(self):
                return real_stdout.flush()

            def close(self):
                return real_stdout.close()

        sys.stdout = FullDiskStdout()
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


class _PinDriver:
    """The shared subprocess driver: the real runner over a temporary tree.

    Not a TestCase — both the abort class and the verdict class below need
    ``run_pin`` / ``counts_line``, and neither can inherit the other's
    tests.
    """

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


class AbortAccountingTests(_PinDriver, unittest.TestCase):
    """Every abort lands in the same accounting: all INVALID, summary, rc 2."""

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


class SurvivedVerdictTests(_PinDriver, unittest.TestCase):
    """The verdict edge no abort can reach: a surviving control exits 1."""

    def test_survived_negative_exits_one_and_keeps_the_tree_restored(self):
        # #1800 fix-r1（PR #2979 boundary P2，升 b 类）：负控回放此前恒为
        # 「rc!=0+marker → KILLED」或 abort，runner 判决路径的 SURVIVED→rc 1
        # 零覆盖——回归把它路由成 rc 0（协调者读成绿）或 rc 2（读成「装置
        # 未跑」），本文件仍全绿，而 runner 自己的契约明写这两个码不得共用。
        process, summary, tree, before = self.run_pin('survived')
        self.assertEqual(process.returncode, 1,
                         f'#1800 a surviving selection must exit 1: '
                         f'{process.stderr!r}{process.stdout!r}')
        self.assertIsNotNone(summary)
        self.assertEqual([row['status'] for row in summary['results']],
                         ['SURVIVED'] * len(SELECTION),
                         '#1800 a green negative run is SURVIVED, never '
                         'KILLED or INVALID')
        self.assertEqual([row['reason'] for row in summary['results']],
                         ['preflight still passed'] * len(SELECTION))
        self.assertEqual(summary['counts'],
                         {'KILLED': 0, 'SURVIVED': len(SELECTION), 'INVALID': 0})
        self.assertEqual(self.counts_line(process.stdout)['counts'],
                         {'KILLED': 0, 'SURVIVED': len(SELECTION), 'INVALID': 0})
        self.assertIn('#1382 SURVIVED controls', process.stderr)
        catalog = (tree / 'src/pilotdeck-gateway/tests/fixtures/sdk-signals'
                   '/negative_controls.py')
        self.assertEqual(catalog.read_bytes(), before,
                         '#1800 a surviving run must still restore the catalog')


class InvalidVerdictTests(_PinDriver, unittest.TestCase):
    """The verdict path's INVALID edge: no verdict, never rc 0 or 1."""

    def test_invalid_negative_exits_two_on_the_verdict_path(self):
        # #1800 fix-r2（PR #2979 boundary P1）：三态契约里 INVALID 的判决路径
        # （非中止）此前零覆盖——本文件每一条 INVALID 行都来自中止分支，而
        # runner 的判决顺序是 abort 判断先于 `if invalid: os._exit(2)`，两条
        # 是不同代码路径。把判决路径的退出码删掉（→rc 0，协调者把「装置没出
        # 判决」读成全绿＝假 PASS）或改成 1（→读成「有控制存活」＝审计误
        # 判），本文件此前全绿；本用例把它钉成 rc 2。
        process, summary, tree, before = self.run_pin('invalid-verdict')
        self.assertEqual(process.returncode, 2,
                         f'#1800 a verdict-path INVALID run must exit 2: '
                         f'{process.stderr!r}{process.stdout!r}')
        self.assertIsNotNone(summary,
                            '#1800 a verdict-path INVALID run is not an '
                            'abort: the summary must still be published')
        self.assertEqual([row['status'] for row in summary['results']],
                         ['INVALID'] * len(SELECTION),
                         '#1800 a negative run without the dedicated #1382 '
                         'marker is INVALID, never KILLED or SURVIVED')
        self.assertEqual([row['reason'] for row in summary['results']],
                         ['preflight failed without the dedicated #1382 '
                          'message'] * len(SELECTION))
        self.assertEqual(summary['counts'],
                         {'KILLED': 0, 'SURVIVED': 0, 'INVALID': len(SELECTION)})
        self.assertEqual(self.counts_line(process.stdout)['counts'],
                         {'KILLED': 0, 'SURVIVED': 0, 'INVALID': len(SELECTION)})
        self.assertIn('#1382 INVALID controls', process.stderr)
        # The rc 2 must come from the verdict path, not the abort branch —
        # the discriminator that keeps the two exit-2 routes from sharing
        # this case's coverage.
        self.assertNotIn('#1382 run did not complete', process.stderr)
        catalog = (tree / 'src/pilotdeck-gateway/tests/fixtures/sdk-signals'
                   '/negative_controls.py')
        self.assertEqual(catalog.read_bytes(), before,
                         '#1800 an INVALID-verdict run must still restore '
                         'the catalog')

    def test_unpublished_summary_exits_two_without_a_summary(self):
        # #1800 fix-r2（PR #2979 boundary P1）：summary 发布失败→rc 2 那条
        # 路径（temp 写入 + rename，失败须 unlink 且不得留半份/旧份 summary）
        # 此前同样零覆盖——本用例让一次全 KILLED 的健康运行只在 rename 一步
        # 失败，钉住「无 summary＝装置没出判决＝rc 2」。
        base = Path(tempfile.mkdtemp(prefix='pdrs-1800-unpublished-'))
        _BASES.append(base)
        out = base / 'out'
        process, summary, tree, before = self.run_pin(
            'summary-publish-fault', base=base)
        self.assertEqual(process.returncode, 2,
                         f'#1800 a run that cannot publish its summary must '
                         f'exit 2: {process.stderr!r}{process.stdout!r}')
        # The rows themselves completed and were recorded as KILLED, so the
        # rc 2 can only come from the publish branch, never an abort.
        self.assertIn(': KILLED: ', process.stdout,
                      '#1800 the publish-fault run must have completed its '
                      'rows: only the summary rename fails')
        self.assertNotIn('#1382 run did not complete', process.stderr)
        self.assertIn('#1382 summary not published: OSError: '
                      '#1800 injected summary publish failure',
                      process.stderr)
        self.assertIsNone(summary,
                           '#1800 an unpublished summary must leave no '
                           'summary.json behind')
        self.assertFalse((out / 'summary.json').exists(),
                         '#1800 a failed publish must not leave a summary '
                         'file a later reader could mistake for evidence')
        self.assertFalse((out / 'summary.json.tmp').exists(),
                         '#1800 a failed publish must drop its temp file')
        catalog = (tree / 'src/pilotdeck-gateway/tests/fixtures/sdk-signals'
                   '/negative_controls.py')
        self.assertEqual(catalog.read_bytes(), before,
                         '#1800 a publish-fault run must still restore '
                         'the catalog')


class StdoutWriteFaultTests(_PinDriver, unittest.TestCase):
    """#1800 fix-r3: a non-EPIPE stdout write failure fails closed, loudly."""

    def test_stdout_write_failure_exits_two_with_a_stderr_diagnostic(self):
        # #1800 fix-r3（评论追加 1）：最终 counts 行的 stdout 写失败——
        # /dev/full 实测抛 OSError(ENOSPC) 而非 BrokenPipeError（实验 A/B
        # 原始输出贴 RESULT）——原 except 只认管道，异常落进尾段 catch-all：
        # rc 2 不变、stderr 0 字节（连 abort 路径的「run did not complete」
        # 都被跳过），即评审所说的「静默 rc 2」。回归两个方向都要红：吞掉
        # 诊断（stderr 无该行），或放行成 rc 0（协调者把没送达的判决读成
        # 全绿）。
        process, summary, tree, before = self.run_pin('stdout-write-fault')
        self.assertEqual(process.returncode, 2,
                         '#1800 a run whose final stdout line cannot be '
                         'written must exit 2 — the verdict line never '
                         f'reached the reader: rc={process.returncode}, '
                         f'stderr={process.stderr!r}')
        self.assertIn(
            '#1382 stdout summary write failed: OSError: '
            '[Errno 28] No space left on device',
            process.stderr,
            '#1800 a swallowed non-EPIPE stdout write failure must name '
            'itself on stderr before failing closed')
        self.assertIsNotNone(
            summary,
            '#1800 only the stdout line failed: the summary must still be '
            'published')
        self.assertTrue(
            [row['status'] for row in summary['results']]
            == ['KILLED'] * len(SELECTION),
            '#1800 the rows completed and were recorded: only the stdout '
            'delivery failed, got '
            + repr([row['status'] for row in summary['results']]))
        self.assertIn(': KILLED: ', process.stdout,
                      '#1800 the record lines must still have reached the '
                      'reader: the fault is the final counts line alone')
        self.assertNotIn('"counts"', process.stdout,
                         '#1800 the counts line must not be readable from '
                         'stdout: its write failed')
        self.assertNotIn('#1382 run did not complete', process.stderr,
                         '#1800 the rc 2 must come from the stdout write '
                         'branch, not the abort branch — the run completed')
        catalog = (tree / 'src/pilotdeck-gateway/tests/fixtures/sdk-signals'
                   '/negative_controls.py')
        self.assertTrue(catalog.read_bytes() == before,
                        '#1800 a stdout-fault run must still restore the '
                        'catalog byte for byte')


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
