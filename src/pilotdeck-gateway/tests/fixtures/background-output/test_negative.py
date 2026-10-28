# anchor-selfcheck: unsupported unit tests of the scratch mutation runner
"""#994 scratch cleanup and exact background failure binding, without cargo."""
import importlib.util
import io
import os
from pathlib import Path
import signal
import subprocess
import sys
import tempfile
import unittest
from contextlib import redirect_stdout
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from _shared import negative_control as control

spec = importlib.util.spec_from_file_location('background_negative', Path(__file__).with_name('negative.py'))
negative = importlib.util.module_from_spec(spec)
spec.loader.exec_module(negative)


class NegativeLifecycleTests(unittest.TestCase):
    def exercise(self, failure=None):
        with tempfile.TemporaryDirectory() as parent:
            target = Path(parent) / 'persistent-target'
            target.mkdir()
            artifact = target / 'cached-artifact'
            artifact.write_text('cache')
            before = {sig: signal.getsignal(sig) for sig in (signal.SIGTERM, signal.SIGINT, signal.SIGALRM)}
            directory = None
            with patch.dict(os.environ, {'CARGO_TARGET_DIR': str(target)}), redirect_stdout(io.StringIO()):
                try:
                    with control.scratch('994-lifecycle', deadline=0) as directory:
                        (directory / 'source').write_text('copy')
                        self.assertEqual(control.persistent_environment(directory)['CARGO_TARGET_DIR'],
                                         str(target.resolve()), '#994 scratch uses caller persistent target')
                        if failure == 'exception':
                            raise RuntimeError('injected copy/build error')
                        if failure == 'interrupt':
                            raise KeyboardInterrupt()
                        if failure == 'sigterm':
                            os.kill(os.getpid(), signal.SIGTERM)
                except (RuntimeError, KeyboardInterrupt, SystemExit):
                    self.assertIsNotNone(failure, '#994 unexpected lifecycle exception')
            self.assertIsNotNone(directory, '#994 scratch must be entered')
            self.assertFalse(directory.exists(), '#994 scratch source copy must be removed')
            self.assertEqual(artifact.read_text(), 'cache', '#994 target must survive scratch cleanup')
            self.assertEqual(before, {sig: signal.getsignal(sig) for sig in before},
                             '#994 restore caller signal handlers')

    def test_cleanup_preserves_persistent_target(self):
        for failure in (None, 'exception', 'interrupt', 'sigterm'):
            with self.subTest(failure=failure):
                self.exercise(failure)


class FailureBindingTests(unittest.TestCase):
    def result(self, name, message, *, thread=None, extra=''):
        test = negative.selection(name)[1][0]
        return subprocess.CompletedProcess([], 101,
            f'running 1 test\ntest {test} ... FAILED\n{extra}\n'
            f"thread '{thread or test}' panicked at example.rs:1:1:\n{message}\n"
            'test result: FAILED. 0 passed; 1 failed;', '')

    def test_expected_assertion_kills(self):
        result = self.result('threshold-minus-one',
            'assertion `left == right` failed: first step #994 background-output oracle comparison')
        self.assertEqual(negative.classify('threshold-minus-one', result, None)[0], 'KILLED',
                         '#994 bound oracle comparison must kill')

    def test_setup_wrong_thread_and_stdout_marker_never_kill(self):
        marker = '#994 background-output oracle comparison'
        for result in (self.result('threshold-minus-one', '#994 background-output setup'),
                       self.result('threshold-minus-one', marker, thread='another_test'),
                       self.result('threshold-minus-one', 'wrong panic', extra=marker)):
            self.assertEqual(negative.classify('threshold-minus-one', result, None)[0], 'INVALID',
                             '#994 unrelated failures cannot count as kills')

    def test_dispose_requires_its_no_snapshot_assertion(self):
        name = 'x-dispose-no-flush-await'
        marker = '#994 dispose-flush must await gated write without snapshot store'
        for message, status in [(marker, 'KILLED'),
                                ('#994 dispose-flush setup gate entered', 'INVALID'),
                                ('#994 dispose-flush setup bounded probe', 'INVALID'),
                                ('#994 dispose-flush completed disk bytes', 'INVALID')]:
            self.assertEqual(negative.classify(name, self.result(name, message), None)[0],
                             status, '#994 dispose must bind the no-snapshot pending assertion')
        for result in (self.result(name, marker, thread='another_test'),
                       self.result(name, 'wrong panic', extra=marker)):
            self.assertEqual(negative.classify(name, result, None)[0], 'INVALID',
                             '#994 dispose rejects unrelated panic/stdout markers')

    def test_dispose_cancelled_write_binding_is_exact(self):
        name = 'x-dispose-no-flush-await'
        bound = name + '-cancelled'
        marker = '#994 dispose-flush must await gated write without snapshot store'
        self.assertEqual(negative.classify(name, self.result(bound, marker), bound)[0], 'KILLED',
                         '#994 cancelled write uses its own full test name')
        self.assertEqual(negative.classify(name, self.result(name, marker), bound)[0], 'INVALID',
                         '#994 pending test cannot stand in for cancelled test')

    def test_runtime_layout_only_accepts_expected_log_failure(self):
        for message, status in [('#994 background-output setup', 'INVALID'),
                                ('#994 background-output expected runtime log', 'KILLED')]:
            self.assertEqual(negative.classify('runtime-layout', self.result('runtime-layout', message), None)[0],
                             status, '#994 layout must bind expected output file, not setup')


if __name__ == '__main__':
    unittest.main(verbosity=2)
