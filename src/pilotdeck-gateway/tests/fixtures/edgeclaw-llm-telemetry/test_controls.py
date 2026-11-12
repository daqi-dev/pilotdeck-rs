# anchor-selfcheck: unsupported runner unit tests using synthetic cargo output and mocked source reads, no product mutation definitions
"""Runner checks use synthetic cargo output; never mutate product sources."""
import importlib.util
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

spec = importlib.util.spec_from_file_location('telemetry_negative', Path(__file__).with_name('negative.py'))
runner = importlib.util.module_from_spec(spec)
spec.loader.exec_module(runner)


class Controls(unittest.TestCase):
    def test_bound_panic_not_expected_data_kills(self):
        case = runner.Case('fixture', None, '', '', 'bound', '#868 bound')
        text = '''running 1 test
test bound ... FAILED
---- bound stdout ----
thread 'bound' panicked at test.rs:1:1:
assertion `left == right` failed: #868 bound
failures:
test result: FAILED. 0 passed; 1 failed;
'''
        result = subprocess.CompletedProcess([], 101, text, '')
        self.assertEqual(runner.classify(result, case)[0], 'KILLED')
        for wrong in [text.replace('failed: #868 bound', 'failed: other\nleft: "#868 bound"'),
                      text.replace("thread 'bound'", "thread 'other'"),
                      text + 'could not compile', text.replace('running 1 test', 'running 0 tests')]:
            self.assertEqual(runner.classify(subprocess.CompletedProcess([], 101, wrong, ''), case)[0], 'INVALID')

    def test_stale_anchor_is_error_and_all_mutants_have_bound_cells(self):
        with patch.object(runner, 'cases', [('stale', Path(__file__), 'NEVER_MATCH_868', '', 'test')]):
            # Avoid matching the token in this test's own source.
            with patch.object(Path, 'read_text', return_value='other source'):
                with self.assertRaises(SystemExit) as stopped:
                    runner.anchors()
                self.assertEqual(stopped.exception.code, 2)
        self.assertEqual(set(runner.cells), {row[0] for row in runner.cases})
        for row in runner.cases:
            self.assertIn(row[4], runner.messages)


class PublicationControls(unittest.TestCase):
    """#1087 fix-r5 (sibling #3): the summary is one rename after restore.

    The rows stay in memory for the whole guarded window and one final
    ``summary.json`` is published by a single rename only after
    ``mutating_sources`` restored and byte-verified the guarded source. An
    abort inside the window voids the recorded rows, publishes INVALID and
    exits 3; a never-started refusal exits 2 and publishes nothing.
    """

    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory(prefix='telemetry-negctl-')
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
        self.name, _path, _old, _new, self.test = runner.cases[0]
        self.assertion = (runner.messages[self.test] + ': '
                          + runner.cells[self.name])

    def green(self, command):
        return subprocess.CompletedProcess(command, 0, '',
            f'running 1 test\ntest {self.test} ... ok\n'
            'test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; '
            '0 filtered out\n')

    def killed(self, command):
        return subprocess.CompletedProcess(command, 101, '',
            'running 1 test\n'
            f'test {self.test} ... FAILED\n\nfailures:\n\n'
            f'---- {self.test} stdout ----\n'
            f"thread '{self.test}' panicked at tests/telemetry.rs:1:1:\n"
            f'assertion `left == right` failed: {self.assertion}\n'
            'note: run with `RUST_BACKTRACE=1` environment variable to '
            'display a backtrace\n'
            'test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; '
            '0 filtered out\n')

    def _drive(self, *, fail_at=None, fail_with=None, restore_fails_at=None,
               before_fails=False, kill=True):
        """Execute the delivered script once with the process layer mocked.

        One selected mutation; ``fail_at`` is the 1-based ``run_owned`` call
        index (1 before, 2 compile, 3 negative, 4 restored — with the
        before-red shape collapsing to before+restored only).
        ``restore_fails_at`` is the 1-based ``write_source_atomic`` call
        index (1 the mutation publish, 2 the in-loop restore).
        """
        out = self.root / 'run'
        state = {'runs': 0, 'writes': 0, 'summary_existed': False}
        disk = {}

        def fake_run_owned(command, *, cwd=None, env=None, timeout=None):
            state['runs'] += 1
            if fail_at == state['runs']:
                raise fail_with
            if state['runs'] == 1:
                if before_fails:
                    return subprocess.CompletedProcess(command, 101, '',
                        f'running 1 test\ntest {self.test} ... FAILED\n'
                        'test result: FAILED. 0 passed; 1 failed; 0 ignored; '
                        '0 measured; 0 filtered out\n')
                return self.green(command)
            if before_fails:
                return self.green(command)  # the restored run
            if state['runs'] == 2:
                return subprocess.CompletedProcess(command, 0, '',
                                                    'Finished `test` profile\n')
            if state['runs'] == 3:
                return self.killed(command) if kill else self.green(command)
            return self.green(command)

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

        def is_summary_tmp(path):
            return (path.parent == out and path.name.startswith('summary.json.')
                    and path.name.endswith('.tmp'))

        class Model:
            real_write_text = Path.write_text
            real_replace = Path.replace

            def model_write_text(self, data, *args, **kwargs):
                if self.parent == out and self.name == 'summary.json':
                    disk['direct'] = data
                    return None
                if is_summary_tmp(self):
                    disk['staged'] = data
                return Model.real_write_text(self, data, *args, **kwargs)

            def model_replace(self, destination, *args, **kwargs):
                if is_summary_tmp(self):
                    disk['renames'] = disk.get('renames', 0) + 1
                    disk['summary'] = disk['staged']
                return Model.real_replace(self, destination, *args, **kwargs)

        source = Path(__file__).with_name('negative.py')
        argv = [str(source), str(out), self.name]
        fake = types.ModuleType('negative_process')
        fake.mutating_sources = fake_mutating
        fake.run_owned = fake_run_owned
        fake.wait_for_slot = fake_slot
        fake.write_source_atomic = fake_atomic
        # #1087 fix-r5: the shared helpers stay the shipped ones so the twins
        # exercise the real staging-name and dead-fd behavior.
        fake.unique_summary_tmp = shared_process.unique_summary_tmp
        fake.detach_dead_streams = shared_process.detach_dead_streams
        saved_process = sys.modules.get('negative_process')
        saved_argv = sys.argv
        sys.modules['negative_process'] = fake
        sys.argv = argv
        try:
            with patch.object(Path, 'write_text', Model.model_write_text), \
                 patch.object(Path, 'replace', Model.model_replace):
                exec(compile(source.read_text(), str(source), 'exec'),
                     {'__name__': '__main__', '__file__': str(source)})
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
        self.assertTrue(payload['controls'][0]['status'] == 'KILLED',
                        message + ' (the final payload must keep the row)')
        self.assertTrue(payload['counts'] == {'KILLED': 1, 'SURVIVED': 0, 'INVALID': 0},
                        message + ' (the counts block must agree)')

    def test_no_summary_is_readable_inside_the_guarded_window(self):
        code, state, disk, out = self._drive()
        message = '#1087 no summary may be readable before restore verification'
        self.assertTrue(code.code == 0, message + ' (the sample run must stay green)')
        self.assertTrue(state['summary_existed'] is False,
                        message + ' (a summary was readable inside the guarded window)')
        self.assertTrue(not list(out.glob('*.tmp')),
                        message + ' (a temp file was left behind)')

    def test_keyboardinterrupt_voids_rows_and_propagates(self):
        code, state, disk, out = self._drive(
            fail_at=3, fail_with=KeyboardInterrupt('red run interrupted'))
        message = '#1087 a cancellation must void the rows and publish INVALID'
        self.assertIsInstance(code, KeyboardInterrupt,
                              message + ' (the cancellation must be re-raised)')
        payload = json.loads((out / 'summary.json').read_text())
        self.assertTrue(payload['controls'][0]['status'] == 'INVALID',
                        message + ' (the on-disk row was not voided)')
        self.assertTrue('runner: KeyboardInterrupt' in payload['controls'][0]['reason'],
                        message + ' (the invalidation reason was not recorded)')

    def test_restore_failure_voids_rows_and_exits_three(self):
        code, state, disk, out = self._drive(restore_fails_at=2)
        message = '#1087 a restore failure must void the rows and exit 3'
        self.assertIsInstance(code, SystemExit,
                              message + ' (the failure must exit through SystemExit)')
        self.assertTrue(code.code == 3, message + ' (a restore failure must exit 3)')
        payload = json.loads((out / 'summary.json').read_text())
        self.assertTrue(payload['controls'][0]['status'] == 'INVALID',
                        message + ' (the on-disk row was not voided)')
        self.assertTrue('runner: RuntimeError' in payload['controls'][0]['reason'],
                        message + ' (the invalidation reason was not recorded)')

    def test_a_surviving_mutant_publishes_and_exits_one(self):
        code, state, disk, out = self._drive(kill=False)
        message = '#1087 a surviving mutant must publish and exit 1'
        self.assertIsInstance(code, SystemExit, message + ' (the run must exit)')
        self.assertTrue(code.code == 1,
                        message + ' (the not-all-killed exit code must stay 1)')
        payload = json.loads(disk['summary'])
        self.assertTrue(payload['controls'][0]['status'] == 'SURVIVED',
                        message + ' (the surviving row must be published, not voided)')

    def test_red_before_run_records_invalid_and_exits_one(self):
        code, state, disk, out = self._drive(before_fails=True)
        message = '#1087 a red before-run must record INVALID and exit 1'
        self.assertIsInstance(code, SystemExit, message + ' (the run must exit)')
        self.assertTrue(code.code == 1,
                        message + ' (the not-all-killed exit code must stay 1)')
        payload = json.loads(disk['summary'])
        self.assertTrue(payload['controls'][0]['status'] == 'INVALID'
                        and payload['controls'][0]['reason'] == 'positive failed',
                        message + ' (the row must record the refused mutation)')

    def test_stale_anchor_is_a_refusal_that_publishes_nothing(self):
        out = self.root / 'refused'
        source = Path(__file__).with_name('negative.py')
        argv = [str(source), str(out), self.name]
        saved_argv = sys.argv
        sys.argv = argv
        # Read the delivered source before the patch: the patch exists to make
        # the runner's own anchor reads stale, not this twin's compile step.
        text = source.read_text()
        try:
            with patch.object(Path, 'read_text', return_value='other source'):
                exec(compile(text, str(source), 'exec'),
                     {'__name__': '__main__', '__file__': str(source)})
        except BaseException as error:
            code = error
        else:  # pragma: no cover - the script always exits
            code = None
        finally:
            sys.argv = saved_argv
        message = '#1087 a stale anchor must be a never-started refusal'
        self.assertIsInstance(code, SystemExit,
                              message + ' (the refusal must exit through SystemExit)')
        self.assertTrue(code.code == 2,
                        message + ' (the stale-anchor refusal must stay exit 2)')
        self.assertTrue(not out.exists() or not list(out.iterdir()),
                        message + ' (a refusal must not publish a summary)')

    def test_missing_target_dir_is_a_refusal_not_a_crash(self):
        saved = os.environ.pop('CARGO_TARGET_DIR', None)
        self.addCleanup(os.environ.__setitem__, 'CARGO_TARGET_DIR', saved)
        code, state, disk, out = self._drive()
        message = '#1087 a missing CARGO_TARGET_DIR must be a never-started refusal'
        self.assertIsInstance(code, SystemExit,
                              message + ' (the refusal must exit through SystemExit)')
        self.assertTrue(code.code == 2,
                        message + ' (the refusal must stay exit 2, never the old '
                                   'string-exit 1 that reads like "ran and red")')
        self.assertTrue(not out.exists() or not list(out.iterdir()),
                        message + ' (a refusal must not publish a summary)')


if __name__ == '__main__':
    unittest.main()
