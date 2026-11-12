# anchor-selfcheck: unsupported unit tests of the kill judge and summary publication, no product source mutations
#!/usr/bin/env python3
"""#1087 fix-r4 controls for the js-math runner's judge and publication.

Two control families over the delivered runner (``negative.py``), neither of
which runs cargo:

* ``JsMathJudgeControls`` — the kill judge is the shared strict #813
  criterion (sibling #3: ``named_test_killed`` plus the ``test result:
  FAILED.`` summary line). The controls pin the positive shape and the
  false-kill shapes — an unrelated panic, a marker only in captured stdout,
  and an aborted run — so a later "simplification" cannot silently drop the
  constraint the old any-``panicked at`` judge missed.
* ``JsMathPublicationControls`` — #1087 fix-r4 (sibling #3/#4): the rows stay
  in memory for the whole guarded window and one final ``summary.json`` is
  published by a single rename only after ``mutating_sources`` restored and
  byte-verified every guarded source. An abort inside the window — a
  cancellation, a restore failure, the source guard — voids the recorded
  rows, publishes INVALID and exits 3. A red baseline keeps exit 1 but still
  publishes its row.

The publication control selects the two router rows (one guarded file, one
baseline target) so the drive stays small while exercising both rows. The
delivered script is a top-level main, so the control executes it a second
time with only the process layer mocked (cargo, slots, guard, source
writes); the bookkeeping under test is the delivered code. The guarded
sources stay on disk untouched: the mocked writes record instead of writing,
and the restore verification reads the same real bytes the guard snapshotted.

Run with:

    python3 -B js-math-zero/test_negative.py
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

from negative_panic import named_test_killed

SELECTED = ('router-zero-max-attempts-nan', 'router-transient-max-clamp')
TEST = 'runtime::model::router::tests::router_retry_policy_uses_js_math_min_max'
ASSERTIONS = {
    'router-zero-max-attempts-nan': '#947 zero_usage Math.max must keep a NaN maxAttempts',
    'router-transient-max-clamp': '#947 transient Math.max must clamp a negative maxAttempts',
}


def libtest_panic_log(payload, *, summary=None):
    """A one-test libtest transcript with the test's own anchor panic."""
    if summary is None:
        summary = ('test result: FAILED. 0 passed; 1 failed; 0 ignored; '
                   '0 measured; 0 filtered out')
    return (
        'running 1 test\n'
        f'test {TEST} ... FAILED\n'
        '\nfailures:\n'
        '\n'
        f'---- {TEST} stdout ----\n'
        f"thread '{TEST}' panicked at src/x.rs:1:1:\n"
        f'{payload}\n'
        'note: run with `RUST_BACKTRACE=1` environment variable to display '
        'a backtrace\n'
        f'{summary}\n'
    )


class JsMathJudgeControls(unittest.TestCase):
    """The js-math kill judge must only accept the named test's own anchor
    panic in a completed run."""

    @classmethod
    def setUpClass(cls):
        source = Path(__file__).with_name('negative.py')
        tree = ast.parse(source.read_text())
        body = [node for node in tree.body
                if isinstance(node, ast.FunctionDef) and node.name == 'classify']
        assert body, 'classify is missing from negative.py'
        namespace = {'re': __import__('re'),
                    'named_test_killed': named_test_killed}
        exec(compile(ast.Module(body=body, type_ignores=[]), str(source), 'exec'),
             namespace)
        cls.classify = staticmethod(namespace['classify'])

    def judge(self, log, returncode=101):
        return self.classify(log, returncode, ASSERTIONS['router-zero-max-attempts-nan'], TEST)

    def test_named_test_anchor_panic_kills(self):
        """The positive shape: the named test's own anchor panic."""
        log = libtest_panic_log(ASSERTIONS['router-zero-max-attempts-nan'])
        self.assertEqual(self.judge(log), 'KILLED',
                         '#1087 the named test\'s own anchor panic must kill')

    def test_survived_shape(self):
        """A green run survives."""
        log = ('running 1 test\n'
               f'test {TEST} ... ok\n'
               'test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; '
               '0 filtered out\n')
        self.assertEqual(self.judge(log, 0), 'SURVIVED',
                         '#1087 a green run survives')

    def test_unrelated_panic_is_not_a_kill(self):
        """The same FAILED shape on an unrelated unwrap is not a kill."""
        log = libtest_panic_log('called `Option::unwrap()` on a None value')
        self.assertEqual(self.judge(log), 'INVALID',
                         '#1087 an unrelated panic in the same binary is not a kill')

    def test_captured_stdout_is_not_a_kill(self):
        """An anchor printed to captured stdout is not a kill."""
        log = libtest_panic_log('called `Option::unwrap()` on a None value').replace(
            f'---- {TEST} stdout ----\n',
            f'---- {TEST} stdout ----\n'
            f'replay dump for debugging: {ASSERTIONS["router-zero-max-attempts-nan"]}\n')
        self.assertEqual(self.judge(log), 'INVALID',
                         '#1087 an anchor only in captured stdout is not a kill')

    def test_aborted_run_without_summary_is_not_a_kill(self):
        """A partial transcript is not a kill.

        The bound test already printed its FAILED line and its own payload
        (the ``--nocapture`` abort shape measured on a real libtest process
        killed by SIGKILL), but the run never reached libtest's own
        ``test result:`` summary. An aborted run is not a kill decision.
        """
        log = libtest_panic_log(ASSERTIONS['router-zero-max-attempts-nan'],
                                summary='')
        self.assertEqual(self.judge(log), 'INVALID',
                         '#1087 an aborted run without the FAILED summary line is not a kill')


class JsMathPublicationControls(unittest.TestCase):
    """The js-math runner must never keep a stale green summary."""

    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory(prefix='jsmath-negctl-')
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

    def _drive(self, *, fail_at=None, fail_with=None, restore_fails_at=None,
               baseline_fails=False):
        """Execute the delivered script once with the process layer mocked.

        The two router rows are selected, so the drive is one baseline run
        (run 1), then per row compile/negative/restored (runs 2-4 and 5-7).
        ``fail_at`` is the 1-based ``run_owned`` call index that raises
        ``fail_with``; ``restore_fails_at`` is the 1-based
        ``write_source_atomic`` call index that raises instead of recording
        (2 and 4 are the in-loop restores, 5 the outer finally's own first
        restore).
        """
        out = (self.root / 'run').resolve()
        state = {'runs': 0, 'writes': 0, 'summary_existed': False}
        disk = {}

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
            name = SELECTED[(state['runs'] - 2) // 3]
            log = libtest_panic_log(ASSERTIONS[name])
            return subprocess.CompletedProcess(command, 101, '', log)

        def fake_mutating(root, paths, **kwargs):
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
        argv = [str(source), str(out), *SELECTED]
        fake = types.ModuleType('negative_process')
        fake.mutating_sources = fake_mutating
        fake.run_owned = fake_run_owned
        fake.wait_for_slot = fake_slot
        fake.write_source_atomic = fake_atomic
        fake.target_environment = lambda root, env=None: None
        # #1087 fix-r5: the shared detach helper stays the shipped one so the
        # twins exercise the real dead-fd detection.
        fake.detach_dead_streams = shared_process.detach_dead_streams
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
        """No summary may exist before the guard restored every source."""
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
        # Write 5 is the outer finally's own first restore (the in-loop
        # restores are writes 2 and 4), so the voiding happens while the
        # guard is unwinding the full frozen table.
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


if __name__ == '__main__':
    unittest.main()
