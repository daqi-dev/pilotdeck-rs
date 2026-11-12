#!/usr/bin/env python3
# anchor-selfcheck: unsupported #2981 sdk-signals abort-accounting meta-control on a temporary tree, no product mutation table
"""#2981: the sdk-signals runner's hand-written abort accounting, pinned.

`sdk-signals/negative_controls.py` is a self-managed loop (it never calls
``negative_suite.run_cases``), so its ``except BaseException`` branch —
revoke every recorded KILLED through the shared ``invalidate_completed``,
record every unrun control as INVALID, still publish the summary, return 2 —
is hand-written, and until this file it had no in-repo test or meta-control
(the #1800 pin covers only the ``sdk-signals-close-order-pin`` sibling). A
regression would surface only as a false PASS with green rows over an
unverified tree (#979 P0), or as a guard fault misread as a verdict.

Every case drives the real runner source in a subprocess whose ``__file__``
points at an identical path inside a temporary tree, so the runner's
self-computed ``root``, its whole mutation table (the anchor preflight
``check_catalog`` must pass over the copy) and both frozen ledgers all land
in the temporary tree. The shared ``negative_suite.invalidate_completed``
and ``publish_summary`` are never mocked — half of the pinned behavior is
that the abort accounting routes through the shared revocation — and only
the ``negative_process`` boundaries are patched in memory, exactly like the
issue-2495 / close-order-pin precedents:

- healthy: the guard is a snapshot/restore stub and ``run_owned`` answers
  the phases from a queue (positive passes, the compile leg is green, each
  control's negative fails at its own bound assertion inside a real-shape
  libtest panic block, the restored run passes). The selection must come out
  all KILLED at rc 0 — the positive control that makes every revoke
  assertion below non-vacuous;
- busy-lock: a real subprocess holds ``<tree>/.negative-source.lock`` through
  the real ``tree_lock``, the runner runs the real ``mutating_sources`` with
  its wait shortened to 2 s (the issue-2495 GUARD_LOCK_FAST shape — this
  runner passes no lock timeout of its own, so the fast bound rides its one
  call site), and the lock timeout must land in the abort accounting: every
  selected row INVALID with the lock path in the reason, rc 2. Unlike the
  close-order pin, ``check_catalog`` runs (and proves the frozen ledgers)
  before the guard opens, so the git read is served from the temporary
  tree's own bytes in this mode too;
- mid-restore-fault: the runner's own restore read-back fails on the last
  control (its ``write_bytes`` restore is skipped, the mutant stays on disk
  and the ``#1145 restoration mismatch`` check fires) — the already recorded
  row must be revoked with its prior diagnostic kept as a prefix, the unrun
  row recorded INVALID, the guard still restores the tree, rc 2;
- guard-exit-fault: the stub guard raises after the body completed — the
  revoke-everything half of the same accounting (#979);
- ledger-drift: ``_head_bytes`` fails on the entry-side frozen-ledger proof
  inside the guard, where ``phase`` is already ``'body'`` — so only the
  ``isinstance(error, FrozenLedgerDrift)`` term of the guard-failure
  predicate can route this run to rc 2. Dropping that term turns the same
  run into an all-INVALID verdict-path rc 1, which this test must catch;
- cancel: ``run_owned`` raises KeyboardInterrupt during the last control's
  negative run — the deterministic shape of a Ctrl-C, which is a
  BaseException the per-control ``except Exception`` must not swallow;
- preflight-drift: ``_head_bytes`` fails on ``check_catalog``'s own ledger
  proof, before any argument parsing — the ``FrozenLedgerDrift`` route in
  ``main`` that prints a diagnostic and returns 2 without creating any
  evidence directory;
- survived: ``run_owned`` answers every phase green — the verdict edge none
  of the abort modes can reach: every row is SURVIVED and the run exits 1;
- invalid-verdict: the negative run fails *without* the bound assertion —
  the shape a stale anchor produces on real hardware — so the row is
  INVALID through the verdict path, not the abort path.

Both non-abort verdict edges pin this runner's current exit contract
(``return int(any(row != KILLED))``): the verdict path's rc 1, distinct from
the abort path's rc 2. That contract differs from the close-order pin's
os._exit tri-state on purpose — the exit-code contract follow-up is
registered as #1866, and when it changes this file changes with it.

The pre-guard refusals (an unknown selection, a missing CARGO_TARGET_DIR, a
missing PDRS_BASELINE) exit 1 with no summary — those run the real,
unmodified script directly, because nothing needs patching before the
guard. Run from this directory:
``python3 -I sdk-signals/test_negative_controls.py``.
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
RUNNER_SCRIPT = FIXTURES / 'sdk-signals' / 'negative_controls.py'
REAL_ROOT = Path(__file__).resolve().parents[5]

# The first per-ID row and a later per-ID row of the registered table: both
# target the shared replay harness, so the mid-restore fault has exactly one
# file whose restore write can be skipped, and both carry the #1252 criterion
# family the close-order pin already keeps registered.
SELECTION = ('unordered-close-run--query-resume-hold-sigint',
             'unordered-close-resume--second-sigterm')
REPLAY_RELATIVE = 'src/pilotdeck-cli/tests/common/signal_replay.rs'
CLOSE_ORDER_RELATIVE = 'specs/gateway-parity/diffs/D1-SDK-WS-CLOSE-ORDER-001.md'
HEALTHY_REASON = 'bound test failed at its assertion'
ABORT_PREFIX = 'source guard or interruption: '
ENTRY_DRIFT = (ABORT_PREFIX + 'FrozenLedgerDrift: #1885 #1382 baseline proof'
               ' failed: ' + CLOSE_ORDER_RELATIVE)


def _catalog_targets():
    """The files the catalog's anchor preflight reads, from its own table.

    Loading the real script (its module level is constants and functions
    only — ``check_catalog`` runs inside ``main``) keeps the temporary tree's
    file set single-sourced: every mutation target plus the read-only
    close-order ledger. A row whose target file is not copied fails the
    temp-tree ``check_catalog`` on the next run, so the copy set cannot
    drift behind the table.
    """
    import importlib.util
    spec = importlib.util.spec_from_file_location(
        'pdrs_2981_sdk_signals_catalog', str(RUNNER_SCRIPT))
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return tuple(sorted(
        path.relative_to(module.root).as_posix()
        for path in {row[1] for row in module.mutations}
        | {module.close_order_ledger}))


RELATIVE_TARGETS = _catalog_targets()

# The subprocess driver. It execs the real runner source with ``__file__``
# inside the temporary tree and patches only the negative_process boundaries
# the busy-lock case must keep real. Mode is the first argv element.
HARNESS_SOURCE = """\
import contextlib
import subprocess
import sys
from pathlib import Path

mode = sys.argv[1]
runner_path = Path(sys.argv[2]).resolve()
real_fixtures = Path(sys.argv[3]).resolve()
tree = Path(sys.argv[4]).resolve()
out = Path(sys.argv[5])
names = sys.argv[6:]

sys.path.insert(0, str(real_fixtures))
import negative_process

# The catalog's own table, loaded from the tree copy before exec: the
# exec'd module only populates its globals while it runs, but the mid-restore
# fault must resolve its target file before the body starts. Importing the
# module is safe — its module level is constants and function defs only
# (check_catalog runs inside main, not at import).
import importlib.util
catalog_spec = importlib.util.spec_from_file_location(
    'pdrs_2981_harness_catalog', str(runner_path))
catalog = importlib.util.module_from_spec(catalog_spec)
catalog_spec.loader.exec_module(catalog)
table = {row[0]: row for row in catalog.mutations}

source = runner_path.read_text(encoding='utf-8')

if mode in ('busy-lock', 'preflight-drift'):
    # Neither mode reaches a fake guard: busy-lock aborts inside the real
    # one, preflight-drift inside check_catalog before it. Both still run
    # check_catalog() first (busy-lock: the ledger proof and the whole
    # 79-anchor preflight; preflight-drift: the ledger proof only), and the
    # temporary tree is not a git checkout, so the git read is served from
    # the tree's own bytes.
    if mode == 'busy-lock':
        # Real guard, real flock timeout: only the wait bound is shortened
        # (the issue-2495 GUARD_LOCK_FAST shape; this runner passes no lock
        # timeout of its own, so the fast bound rides its one call site).
        # The timeout fires before any git work inside the guard, so the
        # temporary tree needs no repository.
        old = 'with mutating_sources(root, guarded) as originals:'
        assert source.count(old) == 1, 'stale mutating_sources call anchor'
        source = source.replace(
            old,
            'with mutating_sources(root, guarded, lock_timeout=2)'
            ' as originals:',
            1)

        def fake_head_bytes(actual_root, relative, _baseline):
            return (Path(actual_root) / relative).read_bytes()
    else:
        def fake_head_bytes(actual_root, relative, _baseline):
            raise OSError('#2981 injected ledger drift')
    negative_process._head_bytes = fake_head_bytes
    runner_globals = {'__file__': str(runner_path), '__name__': '__main__'}
else:
    @contextlib.contextmanager
    def fake_guard(actual_root, requested, **_kwargs):
        snapshots = {Path(path): Path(path).read_bytes() for path in requested}
        try:
            yield snapshots
        finally:
            for path, data in snapshots.items():
                negative_process.write_source_atomic(actual_root, path, data)
            if mode == 'guard-exit-fault':
                raise RuntimeError('#2981 injected guard exit failure')

    calls = {'run': 0, 'head': 0}

    def fake_slot(script, **_kwargs):
        return subprocess.CompletedProcess([], 0, '', '')

    def _row(control):
        return table[names[control]]

    def _ok_log(test):
        return (
            'running 1 test\\n'
            'test ' + test + ' ... ok\\n\\n'
            'test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured;'
            ' 0 filtered out; finished in 0.01s\\n')

    def _failed_log(test, message):
        # The exact libtest shape negative_suite.classify demands: the named
        # FAILED line, the panic payload inside the test's own thread block,
        # and a FAILED summary (negative_panic accepts nothing weaker).
        return (
            'running 1 test\\n'
            'test ' + test + ' ... FAILED\\n\\n'
            'failures:\\n\\n'
            '---- ' + test + ' stdout ----\\n'
            "thread '" + test + "' panicked at"
            ' src/pilotdeck-cli/tests/common/signal_replay.rs:1:1:\\n'
            + message + '\\n'
            'note: run with `RUST_BACKTRACE=1` environment variable to'
            ' display a backtrace\\n\\n'
            'failures:\\n    ' + test + '\\n\\n'
            'test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured;'
            ' 0 filtered out; finished in 0.01s\\n\\n'
            'error: test failed, to rerun pass ' + repr(test))

    def fake_run_owned(command, **_kwargs):
        # The cargo command carries no phase label, so the phases are served
        # from the runner's own deterministic order: positive, compile,
        # negative and restored per selected control (the SessionRunnerTests
        # stage-queue shape).
        index = calls['run']
        calls['run'] += 1
        row = _row(index // 4)
        if mode == 'survived':
            # The verdict edge no abort mode can reach: every phase green,
            # so the negative run still passes and the row survives.
            return subprocess.CompletedProcess(command, 0, _ok_log(row[4]), '')
        phase = index % 4
        if phase in (0, 3):
            return subprocess.CompletedProcess(command, 0, _ok_log(row[4]), '')
        if phase == 1:
            return subprocess.CompletedProcess(command, 0, '', '')
        if mode == 'cancel' and index // 4 == len(names) - 1:
            raise KeyboardInterrupt()
        if mode == 'invalid-verdict':
            # The verdict path's INVALID row: the negative run fails without
            # the bound assertion (the stale-anchor shape), so the runner
            # records INVALID and the exit comes from the verdict path,
            # never the abort branch.
            return subprocess.CompletedProcess(
                command, 1, _failed_log(row[4], 'an unrelated assertion'), '')
        return subprocess.CompletedProcess(
            command, 1, _failed_log(row[4], row[5]), '')

    def fake_head_bytes(actual_root, relative, _baseline):
        index = calls['head']
        calls['head'] += 1
        if mode == 'ledger-drift' and index >= 1:
            # Call 0 was check_catalog's close-order proof; calls 1-2 are the
            # entry-side proof inside the guard, where `phase` is already
            # 'body' and only the FrozenLedgerDrift term of the
            # guard-failure predicate can route the run to rc 2.
            raise OSError('#2981 injected ledger drift')
        return (Path(actual_root) / relative).read_bytes()

    negative_process.mutating_sources = fake_guard
    negative_process.run_owned = fake_run_owned
    negative_process.wait_for_slot = fake_slot
    negative_process._head_bytes = fake_head_bytes

    if mode == 'mid-restore-fault':
        # The runner's own restore check: skip the last control's write-back
        # so the mutant stays on disk and the read-back comparison raises.
        target = _row(len(names) - 1)[1]
        raw_write_bytes = Path.write_bytes
        restore_writes = {'count': 0}

        def skip_last_restore(self, data):
            if self == target:
                restore_writes['count'] += 1
                if restore_writes['count'] == len(names):
                    return len(data)
            return raw_write_bytes(self, data)

        Path.write_bytes = skip_last_restore

    runner_globals = {'__file__': str(runner_path), '__name__': '__main__'}

sys.argv = [str(runner_path), str(out), *names]
exec(compile(source, str(runner_path), 'exec'), runner_globals)
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


class _RunnerDriver:
    """The shared subprocess driver: the real runner over a temporary tree.

    Not a TestCase — both the abort class and the verdict class below need
    ``run_runner`` / ``counts_line``, and neither can inherit the other's
    tests.
    """

    def run_runner(self, mode, names=SELECTION, base=None):
        """Drive the real runner source in a subprocess over a temporary tree.

        Returns ``(process, summary-or-None, tree, target_before, out)``; the
        replay-harness copy is taken before the run so every case can also
        prove the tree was restored whatever the abort did. ``base`` lets a
        caller reuse a tree it prepared (the busy-lock case must lock the
        very tree the runner then runs over).
        """
        if base is None:
            base = Path(tempfile.mkdtemp(prefix='pdrs-2981-pin-'))
            _BASES.append(base)
        tree = base / 'tree'
        for relative in RELATIVE_TARGETS:
            path = tree / relative
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes((REAL_ROOT / relative).read_bytes())
        # The runner itself is redirected too: the subprocess execs the real
        # source but with ``__file__`` pointing at the identical path inside
        # the temporary tree, so the runner's self-computed root (and with it
        # the whole mutation table, both frozen ledgers and the source lock)
        # lands in the temporary tree — a driving bug that left ``__file__``
        # on the real script would aim the real guard's snapshot/restore at
        # the real worktree.
        tree_runner = tree / RUNNER_SCRIPT.relative_to(REAL_ROOT)
        tree_runner.parent.mkdir(parents=True, exist_ok=True)
        tree_runner.write_bytes(RUNNER_SCRIPT.read_bytes())
        harness = base / 'harness.py'
        harness.write_text(HARNESS_SOURCE, encoding='utf-8')
        out = base / 'out'
        before = (tree / REPLAY_RELATIVE).read_bytes()
        env = {**os.environ,
               'CARGO_TARGET_DIR': str(base / 'unused-target'),
               'PDRS_BASELINE': str(base / 'unused-baseline')}
        process = subprocess.run(
            [sys.executable, '-I', '-B', str(harness), mode, str(tree_runner),
             str(FIXTURES), str(tree), str(out), *names],
            capture_output=True, text=True, timeout=120, env=env)
        summary = None
        if (out / 'summary.json').is_file():
            summary = json.loads((out / 'summary.json').read_text(encoding='utf-8'))
        return process, summary, tree, before, out

    @staticmethod
    def counts_line(stdout):
        lines = [line for line in stdout.split('\n') if line.strip()]
        return json.loads(lines[-1]) if lines else None


class AbortAccountingTests(_RunnerDriver, unittest.TestCase):
    """Every abort lands in the same accounting: all INVALID, summary, rc 2."""

    def assert_rows(self, summary, reasons):
        self.assertIsNotNone(summary,
                             '#2981 an aborting run must still publish its summary')
        rows = summary['controls']
        self.assertTrue([row['mutation'] for row in rows] == list(SELECTION),
                        '#2981 every selected control must stay accounted for,'
                        ' got ' + repr([row['mutation'] for row in rows]))
        self.assertTrue([row['status'] for row in rows]
                        == ['INVALID'] * len(SELECTION),
                        '#2981 an aborting run must revoke every recorded'
                        ' kill, got '
                        + repr([row['status'] for row in rows]))
        self.assertTrue([row['reason'] for row in rows] == reasons,
                        '#2981 abort reasons must keep the revoked prefix and'
                        ' name the failure, got '
                        + repr([row['reason'] for row in rows]))
        self.assertTrue([row['compiled'] for row in rows]
                        == [False] * len(SELECTION),
                        '#2981 an aborting run must revoke compilation'
                        ' receipts too, got '
                        + repr([row['compiled'] for row in rows]))
        self.assertTrue(summary['counts']
                        == {'KILLED': 0, 'SURVIVED': 0, 'INVALID': len(SELECTION)},
                        '#2981 an aborting run must count every row INVALID,'
                        ' got ' + repr(summary['counts']))

    def test_healthy_selection_publishes_killed_rows(self):
        process, summary, tree, before, _out = self.run_runner('healthy')
        self.assertEqual(process.returncode, 0,
                         '#2981 a healthy selection must exit 0: '
                         f'{process.stderr!r}{process.stdout!r}')
        self.assertIsNotNone(summary,
                             '#2981 a healthy run must publish its summary')
        rows = summary['controls']
        self.assertTrue([row['status'] for row in rows]
                        == ['KILLED'] * len(SELECTION),
                        '#2981 the driving harness must really produce green'
                        ' rows, or every revoke assertion below is vacuous,'
                        ' got ' + repr([row['status'] for row in rows]))
        self.assertTrue([row['reason'] for row in rows]
                        == [HEALTHY_REASON] * len(SELECTION),
                        '#2981 a healthy kill carries the classify reason,'
                        ' got ' + repr([row['reason'] for row in rows]))
        self.assertTrue([row['compiled'] for row in rows]
                        == [True] * len(SELECTION),
                        '#2981 a healthy kill records its compilation'
                        ' receipt, got '
                        + repr([row['compiled'] for row in rows]))
        self.assertTrue(summary['counts']
                        == {'KILLED': len(SELECTION), 'SURVIVED': 0,
                            'INVALID': 0},
                        '#2981 a healthy run counts every row KILLED, got '
                        + repr(summary['counts']))
        self.assertTrue(self.counts_line(process.stdout)['counts']
                        == {'KILLED': len(SELECTION), 'SURVIVED': 0,
                            'INVALID': 0},
                        '#2981 the final stdout counts line must agree with'
                        ' the summary, got '
                        + repr(self.counts_line(process.stdout)))
        self.assertTrue((tree / REPLAY_RELATIVE).read_bytes() == before,
                        '#2981 a healthy run must restore its mutation target'
                        ' byte for byte')

    def test_busy_source_lock_records_every_row_invalid(self):
        base = Path(tempfile.mkdtemp(prefix='pdrs-2981-lock-'))
        _BASES.append(base)
        tree = base / 'tree'
        tree.mkdir(parents=True)
        ready = base / 'held'
        holder = subprocess.Popen(
            [sys.executable, '-I', '-B', '-c', LOCK_HOLDER_SOURCE,
             str(FIXTURES), str(tree), str(ready)],
            start_new_session=True)
        self.addCleanup(self._kill_holder, holder)
        deadline = time.monotonic() + 30
        while not ready.exists():
            self.assertIsNone(
                holder.poll(),
                '#2981 the lock holder died before holding the tree lock')
            self.assertLess(time.monotonic(), deadline,
                            '#2981 the lock holder never published its ready'
                            ' file')
            time.sleep(0.05)
        process, summary, tree_back, before, _out = self.run_runner(
            'busy-lock', base=base)
        self.assertEqual(
            process.returncode, 2,
            '#2981 a busy source lock must abort at rc 2, got '
            f'{process.returncode}: {process.stderr!r}{process.stdout!r}')
        busy_reason = (ABORT_PREFIX + 'TimeoutError: source lock busy: '
                      + str((tree / '.negative-source.lock').resolve()))
        # resolve()：runner 从已解析的 __file__ 自算 root（macOS 上 mkdtemp 给
        # /var/folders、resolve 后是 /private/var/folders，不解析即假红）
        self.assert_rows(summary, [busy_reason] * len(SELECTION))
        self.assertTrue(self.counts_line(process.stdout)['counts']['INVALID']
                        == len(SELECTION),
                        '#2981 the final stdout counts line must agree with'
                        ' the INVALID summary, got '
                        + repr(self.counts_line(process.stdout)))
        self.assertTrue((tree_back / REPLAY_RELATIVE).read_bytes() == before,
                        '#2981 an entry-phase abort never wrote the mutation'
                        ' target')

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
        # The #2981 core: a restore failure after a green row must revoke the
        # kill (keeping its diagnostic as the prefix, the #979 shape) and
        # still record the unrun control — never a green row over an
        # unverified tree.
        process, summary, tree, before, _out = self.run_runner(
            'mid-restore-fault')
        self.assertEqual(process.returncode, 2,
                         '#2981 a mid-run restore failure must exit 2: '
                         f'{process.stderr!r}{process.stdout!r}')
        failure = (ABORT_PREFIX + 'RuntimeError: #1145 restoration mismatch: '
                   + REPLAY_RELATIVE)
        self.assert_rows(summary, [HEALTHY_REASON + '; ' + failure, failure])
        self.assertTrue((tree / REPLAY_RELATIVE).read_bytes() == before,
                        '#2981 the guard must still restore the tree its'
                        ' runner failed to')

    def test_guard_exit_failure_revokes_every_completed_row(self):
        process, summary, tree, before, _out = self.run_runner(
            'guard-exit-fault')
        self.assertEqual(process.returncode, 2,
                         '#2981 a guard exit failure must exit 2: '
                         f'{process.stderr!r}{process.stdout!r}')
        failure = ABORT_PREFIX + 'RuntimeError: #2981 injected guard exit failure'
        self.assert_rows(summary, [HEALTHY_REASON + '; ' + failure,
                                   HEALTHY_REASON + '; ' + failure])
        self.assertTrue((tree / REPLAY_RELATIVE).read_bytes() == before,
                        '#2981 a guard-exit fault still restores before it'
                        ' raises')

    def test_entry_ledger_drift_revokes_every_row_on_the_body_phase(self):
        # The one abort whose `phase` is 'body': the guard opened, the loop
        # never ran, and the entry-side frozen-ledger proof raised
        # FrozenLedgerDrift. Only the isinstance term of the guard-failure
        # predicate can route it to rc 2 — dropping that term turns the same
        # run into the verdict path's rc 1 over revoked rows.
        process, summary, tree, before, _out = self.run_runner('ledger-drift')
        self.assertEqual(process.returncode, 2,
                         '#2981 a body-phase FrozenLedgerDrift must exit 2: '
                         f'{process.stderr!r}{process.stdout!r}')
        self.assert_rows(summary, [ENTRY_DRIFT, ENTRY_DRIFT])
        self.assertTrue((tree / REPLAY_RELATIVE).read_bytes() == before,
                        '#2981 a body-phase ledger drift never wrote the'
                        ' mutation target')

    def test_cancellation_voids_recorded_kills_and_unrun_rows(self):
        # KeyboardInterrupt is a BaseException the per-control ``except
        # Exception`` must not swallow: the accounting sees it as a
        # cancellation of the whole run.
        process, summary, tree, before, _out = self.run_runner('cancel')
        self.assertEqual(process.returncode, 2,
                         '#2981 a cancellation must exit 2: '
                         f'{process.stderr!r}{process.stdout!r}')
        cancel_reason = ABORT_PREFIX + 'KeyboardInterrupt: '
        self.assert_rows(summary,
                         [HEALTHY_REASON + '; ' + cancel_reason,
                          cancel_reason])
        self.assertTrue((tree / REPLAY_RELATIVE).read_bytes() == before,
                        '#2981 a cancelled run must still restore the tree')


class VerdictEdgeTests(_RunnerDriver, unittest.TestCase):
    """The verdict edges no abort can reach: non-KILLED rows exit 1.

    This runner's verdict path is ``return int(any(row != KILLED))`` — both
    SURVIVED and INVALID rows share rc 1, distinct from the abort path's
    rc 2. The exit-code contract follow-up is registered as #1866; this pins
    the current behavior so any change there is deliberate.
    """

    def test_survived_negative_exits_one_and_keeps_the_tree_restored(self):
        process, summary, tree, before, _out = self.run_runner('survived')
        self.assertEqual(process.returncode, 1,
                         '#2981 a surviving selection must exit 1, not 0 (a'
                         ' coordinator would read a red device as green)'
                         f': {process.stderr!r}{process.stdout!r}')
        self.assertIsNotNone(summary,
                             '#2981 a surviving run is not an abort: the'
                             ' summary must still be published')
        rows = summary['controls']
        self.assertTrue([row['status'] for row in rows]
                        == ['SURVIVED'] * len(SELECTION),
                        '#2981 a green negative run is SURVIVED, never'
                        ' KILLED or INVALID, got '
                        + repr([row['status'] for row in rows]))
        self.assertTrue([row['reason'] for row in rows]
                        == ['bound test still passed'] * len(SELECTION),
                        '#2981 a surviving row carries the classify reason,'
                        ' got ' + repr([row['reason'] for row in rows]))
        self.assertTrue(summary['counts']
                        == {'KILLED': 0, 'SURVIVED': len(SELECTION),
                            'INVALID': 0},
                        '#2981 a surviving run counts every row SURVIVED,'
                        ' got ' + repr(summary['counts']))
        self.assertTrue(self.counts_line(process.stdout)['counts']
                        == {'KILLED': 0, 'SURVIVED': len(SELECTION),
                            'INVALID': 0},
                        '#2981 the final stdout counts line must agree with'
                        ' the summary, got '
                        + repr(self.counts_line(process.stdout)))
        self.assertTrue((tree / REPLAY_RELATIVE).read_bytes() == before,
                        '#2981 a surviving run must still restore the tree')

    def test_invalid_negative_exits_one_on_the_verdict_path(self):
        # The negative run fails without the bound assertion (the
        # stale-anchor shape), so the rows are INVALID through the verdict
        # path — rc 1, the code this runner reserves for "the device ran and
        # produced a non-kill", never the abort path's rc 2.
        process, summary, tree, before, _out = self.run_runner(
            'invalid-verdict')
        self.assertEqual(process.returncode, 1,
                         '#2981 a verdict-path INVALID run must exit 1: '
                         f'{process.stderr!r}{process.stdout!r}')
        self.assertIsNotNone(summary,
                             '#2981 a verdict-path INVALID run is not an'
                             ' abort: the summary must still be published')
        rows = summary['controls']
        self.assertTrue([row['status'] for row in rows]
                        == ['INVALID'] * len(SELECTION),
                        '#2981 a negative run without the bound assertion is'
                        ' INVALID, never KILLED or SURVIVED, got '
                        + repr([row['status'] for row in rows]))
        self.assertTrue([row['reason'] for row in rows]
                        == ['missing bound runtime failure (build error,'
                            ' wrong panic, or missing test)'] * len(SELECTION),
                        '#2981 an INVALID verdict row carries the classify'
                        ' reason, got '
                        + repr([row['reason'] for row in rows]))
        self.assertTrue((tree / REPLAY_RELATIVE).read_bytes() == before,
                        '#2981 an INVALID-verdict run must still restore the'
                        ' tree')


class PreflightLedgerDriftTests(_RunnerDriver, unittest.TestCase):
    """check_catalog's own FrozenLedgerDrift route: rc 2, no evidence at all."""

    def test_preflight_ledger_drift_exits_two_without_evidence(self):
        process, _summary, _tree, _before, out = self.run_runner(
            'preflight-drift')
        self.assertEqual(process.returncode, 2,
                         '#2981 a preflight ledger drift must exit 2, got '
                         f'{process.returncode}: {process.stderr!r}'
                         f'{process.stdout!r}')
        self.assertIn(ENTRY_DRIFT, process.stderr,
                      '#2981 the preflight failure must name itself on'
                      ' stderr before returning 2')
        self.assertFalse(out.exists(),
                         '#2981 a preflight guard failure must not create an'
                         ' evidence directory a later reader could mistake'
                         ' for a run')


class PreGuardRefusalTests(unittest.TestCase):
    """The other side of the exit contract: pre-guard refusals exit 1."""

    def refusal(self, argv, env):
        base = Path(tempfile.mkdtemp(prefix='pdrs-2981-refusal-'))
        _BASES.append(base)
        out = base / 'out'
        result = subprocess.run(
            [sys.executable, '-I', '-B', str(RUNNER_SCRIPT), str(out), *argv],
            capture_output=True, text=True, timeout=60, env=env)
        return result, out

    def test_unknown_selection_exits_one_without_a_summary(self):
        result, out = self.refusal(
            ['no-such-control-2981'],
            {**os.environ, 'CARGO_TARGET_DIR': '/nonexistent-cargo-target'})
        self.assertEqual(result.returncode, 1,
                         f'#2981 an unknown selection is a pre-guard refusal'
                         f' (rc 1): {result.stderr!r}')
        self.assertIn('#1145 unknown or duplicate mutation selection',
                      result.stderr)
        self.assertFalse(out.exists(),
                         '#2981 a pre-guard refusal must not create evidence')

    def test_missing_cargo_target_exits_one_without_a_summary(self):
        result, out = self.refusal(
            [SELECTION[0]],
            {key: value for key, value in os.environ.items()
             if key != 'CARGO_TARGET_DIR'})
        self.assertEqual(result.returncode, 1,
                         f'#2981 a missing CARGO_TARGET_DIR is a pre-guard'
                         f' refusal (rc 1): {result.stderr!r}')
        self.assertIn('#1145 CARGO_TARGET_DIR must be supplied', result.stderr)
        self.assertFalse(out.exists())

    def test_missing_baseline_exits_one_without_a_summary(self):
        env = {key: value for key, value in os.environ.items()
               if key not in ('CARGO_TARGET_DIR', 'PDRS_BASELINE')}
        env['CARGO_TARGET_DIR'] = '/nonexistent-cargo-target'
        result, out = self.refusal([SELECTION[0]], env)
        self.assertEqual(result.returncode, 1,
                         f'#2981 a missing PDRS_BASELINE is a pre-guard'
                         f' refusal (rc 1): {result.stderr!r}')
        self.assertIn('#1145 PDRS_BASELINE must identify the real TS checkout',
                      result.stderr)
        self.assertFalse(out.exists())


def tearDownModule():
    for base in _BASES:
        shutil.rmtree(base, ignore_errors=True)


if __name__ == '__main__':
    unittest.main()
