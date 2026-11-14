# anchor-selfcheck: unsupported unit tests of the summary publication, no product source mutations
#!/usr/bin/env python3
"""#1087 fix-r4 controls for the task-budget-zero runner.

Two control families over the delivered runner (``negative.py``), neither of
which runs cargo:

* ``TaskBudgetJudgeControls`` — the kill judge already requires the FAILED
  summary line (fix-r3); the controls pin the positive and false-kill shapes
  so a later "simplification" cannot silently drop the constraint.
* ``TaskBudgetPublicationControls`` — #1087 fix-r4 (sibling #3): the rows
  stay in memory for the whole guarded window and one final ``summary.json``
  is published by a single rename only after ``mutating_sources`` restored
  and byte-verified the guarded source. An abort inside the window — a
  cancellation, a restore failure, the source guard — voids the recorded
  rows, publishes INVALID and exits 3 (never 2: that is the "never started"
  usage refusal). A red baseline keeps exit 1 but still publishes its row.

The delivered script is a top-level main, so the publication control executes
it a second time with only the process layer mocked (cargo, slots, guard,
source writes); the bookkeeping under test is the delivered code. The
guarded source stays on disk untouched: the mocked writes record instead of
writing, and the restore verification reads the same real bytes the guard
snapshotted.

Run with:

    python3 -B task-budget-zero/test_negative.py
"""
import ast
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import types
import unittest
from unittest.mock import patch

_FIXTURES = Path(__file__).resolve().parents[1]
if str(_FIXTURES) not in sys.path:
    sys.path.insert(0, str(_FIXTURES))
import negative_process as shared_process

FIXTURES = Path(__file__).resolve().parents[1]
if str(FIXTURES) not in sys.path:
    sys.path.insert(0, str(FIXTURES))


def load_table():
    """Read ``cases``/``ASSERTIONS``/``TEST`` from the delivered script."""
    source = Path(__file__).with_name('negative.py')
    tree = ast.parse(source.read_text())
    table = assertions = test = None
    for node in tree.body:
        if not (isinstance(node, ast.Assign)
                and isinstance(node.targets[0], ast.Name)):
            continue
        name = node.targets[0].id
        if name == 'cases' and table is None and isinstance(node.value, ast.List):
            table = [ast.literal_eval(item.elts[0])
                     for item in node.value.elts]
        elif name == 'ASSERTIONS' and assertions is None:
            assertions = ast.literal_eval(node.value)
        elif name == 'TEST' and test is None:
            test = ast.literal_eval(node.value)
    assert table and assertions and test, \
        'negative.py must keep module-level cases/ASSERTIONS/TEST tables'
    return table, assertions, test


TABLE, ASSERTIONS, TEST = load_table()


class TaskBudgetJudgeControls(unittest.TestCase):
    """The task-budget kill judge must only accept a completed failing run."""

    @classmethod
    def setUpClass(cls):
        source = Path(__file__).with_name('negative.py')
        tree = ast.parse(source.read_text())
        body = [node for node in tree.body
                if isinstance(node, ast.FunctionDef) and node.name == 'classify']
        assert body, 'classify is missing from negative.py'
        namespace = {'re': __import__('re')}
        exec(compile(ast.Module(body=body, type_ignores=[]), str(source), 'exec'),
             namespace)
        cls.classify = staticmethod(namespace['classify'])

    def test_positive_killed_and_survived_shapes(self):
        """The three-way classify keeps its positive shapes."""
        killed = ('running 1 test\n'
                  f'test {TEST} ... FAILED\n'
                  'failures:\n'
                  f"thread '{TEST}' panicked at tests/task_budget.rs:9:9:\n"
                  'must stay NaN like TS Math.max\n'
                  'test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out\n')
        survived = ('running 1 test\n'
                    f'test {TEST} ... ok\n'
                    'test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out\n')
        self.assertEqual(self.classify(killed, 101), 'KILLED',
                         '#1087 a completed FAILED run with the panic is a kill')
        self.assertEqual(self.classify(survived, 0), 'SURVIVED',
                         '#1087 a green run survives')

    def test_aborted_run_without_summary_is_not_a_kill(self):
        """A partial transcript is not a kill.

        The bound test already printed its FAILED line and its own payload
        (the ``--nocapture`` abort shape measured on a real libtest process
        killed by SIGKILL), but the run never reached libtest's own
        ``test result:`` summary. An aborted run is not a kill decision.
        """
        aborted = ('running 1 test\n'
                   f'test {TEST} ... FAILED\n'
                   'failures:\n'
                   f"thread '{TEST}' panicked at tests/task_budget.rs:9:9:\n"
                   'must stay NaN like TS Math.max\n')
        self.assertEqual(self.classify(aborted, 101), 'INVALID',
                         '#1087 an aborted run without the FAILED summary line is not a kill')


class TaskBudgetPublicationControls(unittest.TestCase):
    """The task-budget runner must never keep a stale green summary."""

    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory(prefix='taskbudget-negctl-')
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name)
        # The delivered script refuses to run without a caller-owned build tree.
        self.saved_target = os.environ.get('CARGO_TARGET_DIR')
        os.environ['CARGO_TARGET_DIR'] = str(self.root / 'target')
        if self.saved_target is None:
            self.addCleanup(os.environ.pop, 'CARGO_TARGET_DIR', None)
        else:
            self.addCleanup(os.environ.__setitem__, 'CARGO_TARGET_DIR',
                            self.saved_target)
        self.selected = list(TABLE)

    def _drive(self, *, fail_at=None, fail_with=None, restore_fails_at=None,
               baseline_fails=False, stale_anchors=False):
        """Execute the delivered script once with the process layer mocked.

        ``fail_at`` is the 1-based ``run_owned`` call index (1 is the
        baseline run) that raises ``fail_with``; ``restore_fails_at`` is the
        1-based ``write_source_atomic`` call index that raises instead of
        recording (2 is the first row's in-loop restore, 5 the outer
        finally's own restore).
        ``stale_anchors`` snapshots guard bytes that contain none of the
        anchors, so the delivered in-window check refuses before any cargo
        run.
        """
        # claim_dir resolves the path, and on this host /var is a symlink to
        # /private/var, so the model must compare against the resolved form.
        out = (self.root / 'run').resolve()
        state = {'runs': 0, 'writes': 0, 'summary_existed': False}
        disk = {}
        selected = self.selected

        def fake_run_owned(command, cwd, env, timeout):
            state['runs'] += 1
            if fail_at == state['runs']:
                raise fail_with
            label = {1: 'baseline', 2: 'compile', 3: 'negative', 4: 'restored',
                     5: 'compile', 6: 'negative', 7: 'restored'}[state['runs']]
            if label == 'baseline' and baseline_fails:
                log = ('running 1 test\n'
                       f'test {TEST} ... FAILED\n'
                       'test result: FAILED. 0 passed; 1 failed; 0 ignored; '
                       '0 measured; 0 filtered out\n')
                return subprocess.CompletedProcess(command, 101, '', log)
            if label in ('baseline', 'restored'):
                log = ('running 1 test\n'
                       f'test {TEST} ... ok\n'
                       'test result: ok. 1 passed; 0 failed; 0 ignored; '
                       '0 measured; 0 filtered out\n')
                return subprocess.CompletedProcess(command, 0, '', log)
            if label == 'compile':
                return subprocess.CompletedProcess(command, 0, '',
                                                    'Finished `test` profile\n')
            name = selected[(state['runs'] - 2) // 3]
            log = ('running 1 test\n'
                   f'test {TEST} ... FAILED\n'
                   '\nfailures:\n'
                   f"thread '{TEST}' (2841) panicked at tests/task_budget.rs:12:5:\n"
                   f'{ASSERTIONS[name]}\n'
                   'note: run with `RUST_BACKTRACE=1` environment variable to display '
                   'a backtrace\n'
                   'test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; '
                   '0 filtered out\n')
            return subprocess.CompletedProcess(command, 101, '', log)

        def fake_mutating(root, paths, **kwargs):
            if stale_anchors:
                originals = {path: b'fn unrelated() {}' for path in paths}
            else:
                originals = {path: path.read_bytes() for path in paths}

            class Guard:
                def __enter__(self):
                    return originals

                def __exit__(self, *exc):
                    return False

            return Guard()

        def fake_slot(script, **kwargs):
            return types.SimpleNamespace(stdout='', stderr='')

        def fake_atomic(root, path, data, *, mode=None):
            state['writes'] += 1
            state['summary_existed'] = (state['summary_existed']
                                        or (out / 'summary.json').exists())
            if restore_fails_at is not None and state['writes'] == restore_fails_at:
                raise RuntimeError('twin restore failure')

        class Model:
            real_write_text = Path.write_text
            real_replace = Path.replace

            def model_write_text(self, data, *args, **kwargs):
                if self.parent == out and self.name == 'summary.json':
                    disk['direct'] = data
                    return None
                if self.parent == out and self.name == 'summary.json.tmp':
                    disk['staged'] = data
                return Model.real_write_text(self, data, *args, **kwargs)

            def model_replace(self, destination, *args, **kwargs):
                if self.parent == out and self.name == 'summary.json.tmp':
                    disk['renames'] = disk.get('renames', 0) + 1
                    disk['summary'] = disk['staged']
                return Model.real_replace(self, destination, *args, **kwargs)

        source = Path(__file__).with_name('negative.py')
        argv = [str(source), str(out)] + selected
        fake = types.ModuleType('negative_process')
        fake.mutating_sources = fake_mutating
        fake.run_owned = fake_run_owned
        fake.wait_for_slot = fake_slot
        fake.write_source_atomic = fake_atomic
        fake.target_environment = lambda root, env=None: None
        # #1087 fix-r5: the shared detach helper stays the shipped one so the
        # twins exercise the real dead-fd detection.
        fake.detach_dead_streams = shared_process.detach_dead_streams
        # #1087 fix-r8 (S1-a): the guarded prints now go through the shared
        # original; the fake keeps the shipped one.
        fake.dead_stream_print = shared_process.dead_stream_print
        saved_process = sys.modules.get('negative_process')
        saved_argv = sys.argv
        sys.modules['negative_process'] = fake
        sys.argv = argv
        try:
            with patch.object(Path, 'write_text', Model.model_write_text), \
                 patch.object(Path, 'replace', Model.model_replace):
                exec(compile(source.read_text(), str(source), 'exec'),
                     {'__name__': 'negative_publication_twin',
                      '__file__': str(source)})
        except BaseException as error:  # the delivered script always exits
            code = error
        else:  # pragma: no cover - the script never falls off the end
            code = None
        finally:
            sys.argv = saved_argv
            if saved_process is None:
                sys.modules.pop('negative_process', None)
            else:
                sys.modules['negative_process'] = saved_process
        return code, state, disk, out

    def test_green_run_publishes_via_one_rename(self):
        """The final summary is one rename, never an in-place write."""
        code, state, disk, out = self._drive()
        message = '#1087 the final summary must be one rename, never an in-place write'
        self.assertIsInstance(code, SystemExit,
                              message + ' (the sample run must exit cleanly)')
        self.assertTrue(code.code == 0, message + ' (the sample run must stay green)')
        self.assertTrue(disk.get('direct') is None,
                        message + ' (summary.json was written in place)')
        self.assertTrue(disk.get('renames') == 1,
                        message + ' (the summary was not published by exactly one rename)')
        payload = json.loads(disk['summary'])
        self.assertTrue(payload['baseline']['outcome'] == 'PASS',
                        message + ' (the baseline row must record the green run)')
        self.assertTrue([row['status'] for row in payload['cases']]
                        == ['KILLED', 'KILLED'],
                        message + ' (the final payload must keep every row)')

    def test_no_summary_is_readable_inside_the_guarded_window(self):
        """No summary may exist before the guard restored the tree."""
        code, state, disk, out = self._drive()
        message = '#1087 no summary may be readable before restore verification'
        self.assertTrue(code.code == 0, message + ' (the sample run must stay green)')
        self.assertTrue(state['summary_existed'] is False,
                        message + ' (a summary was readable inside the guarded window)')
        self.assertTrue(not list(out.glob('*.tmp')),
                        message + ' (a temp file was left behind)')

    def test_keyboardinterrupt_voids_rows_and_propagates(self):
        """A mid-suite cancellation voids every row, then propagates."""
        # Run 6 is the second row's negative run: the first row is already
        # recorded KILLED, so the voiding of a recorded row is exercised too.
        code, state, disk, out = self._drive(
            fail_at=6, fail_with=KeyboardInterrupt('red run interrupted'))
        message = '#1087 a cancellation must void the rows and publish INVALID'
        self.assertIsInstance(code, KeyboardInterrupt,
                              message + ' (the cancellation must be re-raised)')
        payload = json.loads((out / 'summary.json').read_text())
        self.assertTrue([row['status'] for row in payload['cases']]
                        == ['INVALID', 'INVALID'],
                        message + ' (the on-disk rows were not voided)')
        self.assertTrue(
            all('runner: KeyboardInterrupt' in row['reason']
                for row in payload['cases']),
            message + ' (the invalidation reason was not recorded)')
        self.assertTrue('voided' in payload['cases'][0]['reason'],
                        message + ' (the already-KILLED row must be voided too)')

    def test_systemexit_is_normalized_to_three(self):
        """An aborting SystemExit pairs with exit code 3, never 2 or the raw code."""
        code, state, disk, out = self._drive(fail_at=3, fail_with=SystemExit(130))
        message = '#1087 an INVALID summary must pair with exit code 3'
        self.assertIsInstance(code, SystemExit,
                              message + ' (the SystemExit must propagate)')
        self.assertTrue(code.code == 3,
                        message + ' (the raw code must be normalized away from 2)')
        payload = json.loads((out / 'summary.json').read_text())
        self.assertTrue([row['status'] for row in payload['cases']]
                        == ['INVALID', 'INVALID'],
                        message + ' (the on-disk rows were not voided)')

    def test_baseline_failure_publishes_and_exits_one(self):
        """A red baseline publishes its row and keeps exit code 1."""
        code, state, disk, out = self._drive(baseline_fails=True)
        message = '#1087 a failed baseline must publish and exit 1'
        self.assertIsInstance(code, SystemExit, message + ' (the run must exit)')
        self.assertTrue(code.code == 1,
                        message + ' (the baseline-failure exit code must stay 1)')
        payload = json.loads((out / 'summary.json').read_text())
        self.assertTrue(payload['baseline']['outcome'] == 'FAIL',
                        message + ' (the baseline row must record the failure)')
        self.assertTrue(payload['cases'] == [],
                        message + ' (no mutant may run after a red baseline)')

    def test_restore_failure_voids_rows_and_exits_three(self):
        """A guard failure voids the rows and exits 3."""
        code, state, disk, out = self._drive(restore_fails_at=5)
        message = '#1087 a guard failure must void the rows and exit 3'
        self.assertIsInstance(code, SystemExit,
                              message + ' (the failure must exit through SystemExit)')
        self.assertTrue(code.code == 3, message + ' (a guard failure must exit 3)')
        payload = json.loads((out / 'summary.json').read_text())
        self.assertTrue([row['status'] for row in payload['cases']]
                        == ['INVALID', 'INVALID'],
                        message + ' (the on-disk rows were not voided)')
        self.assertTrue(
            all('runner: RuntimeError' in row['reason'] for row in payload['cases']),
            message + ' (the invalidation reason was not recorded)')

    def test_closed_stderr_keeps_the_abort_exit_code(self):
        """#1087 fix-r4 (concurrency #2): a closed stderr must not replace
        the exit code.

        The summary is already published when the diagnostic print runs, so
        an unwritable stderr (a host that closed `2>&-`) must not turn the
        "aborted = 3" contract into an unhandled OSError exit 1.
        """
        class ClosedStderr:
            def write(self, data):
                raise OSError('stderr is closed')

            def flush(self):
                pass

        with patch.object(sys, 'stderr', ClosedStderr()):
            code, state, disk, out = self._drive(fail_at=3, fail_with=SystemExit(130))
        message = '#1087 a closed stderr must not replace the exit code'
        self.assertIsInstance(code, SystemExit,
                              message + ' (the SystemExit must propagate)')
        self.assertTrue(code.code == 3,
                        message + ' (the raw code must be normalized away from 2)')
        payload = json.loads((out / 'summary.json').read_text())
        self.assertTrue([row['status'] for row in payload['cases']]
                        == ['INVALID', 'INVALID'],
                        message + ' (the on-disk rows were not voided)')

    def test_stale_anchor_refusal_survives_a_dead_stderr(self):
        """#1087 fix-r7 batch 1 (P1b): a dead stderr keeps the refusal at exit 2.

        ``2>&-`` or a closed stream makes the in-window STALE ANCHOR prints
        raise; the old unguarded prints turned the never-started refusal into
        an in-window abort (exit 3 with a voided INVALID summary published).
        """
        class ClosedStderr:
            def write(self, data):
                raise OSError('stderr is closed')

            def flush(self):
                pass

        with patch.object(sys, 'stderr', ClosedStderr()):
            code, state, disk, out = self._drive(stale_anchors=True)
        message = '#1087 a dead stderr must keep the stale-anchor refusal at exit 2'
        self.assertIsInstance(code, SystemExit,
                              message + ' (the refusal must exit through SystemExit)')
        self.assertTrue(code.code == 2,
                        message + ' (the refusal must stay exit 2, not abort 3)')
        self.assertTrue(not (out / 'summary.json').exists(),
                        message + ' (a refusal must not publish a summary)')

    def test_check_anchors_ok_survives_a_dead_stdout(self):
        """#1087 fix-r7 batch 1 (P1b): the green --check keeps exit 0 on a dead stdout.

        ``1>&-`` or a closed stream makes the "anchors: ok" print raise; the
        old unguarded print turned a green read-only ``--check`` into a crash
        (rc=1, "ran and was red") instead of the contract's exit 0.
        """
        import negative_selection  # noqa: F401 - import before any patch

        class ClosedStdout:
            def write(self, data):
                raise OSError('stdout is closed')

            def flush(self):
                pass

        source = Path(__file__).with_name('negative.py')
        text = source.read_text()
        saved_argv = sys.argv
        sys.argv = [str(source), '--check']
        try:
            with patch.object(sys, 'stdout', ClosedStdout()):
                try:
                    exec(compile(text, str(source), 'exec'),
                         {'__name__': 'task_budget_check_twin',
                          '__file__': str(source)})
                except BaseException as error:  # the preflight always exits
                    code = error
                else:  # pragma: no cover - the preflight never falls through
                    code = None
        finally:
            sys.argv = saved_argv
        message = '#1087 a dead stdout must keep the green --check at exit 0'
        self.assertIsInstance(code, SystemExit,
                              message + ' (the check must exit through SystemExit)')
        self.assertTrue(code.code == 0,
                        message + ' (a dead stdout must not turn it into rc=1)')


if __name__ == '__main__':
    unittest.main()
