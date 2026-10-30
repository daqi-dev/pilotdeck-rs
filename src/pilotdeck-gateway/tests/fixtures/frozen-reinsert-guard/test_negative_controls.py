#!/usr/bin/env python3
# anchor-selfcheck: unsupported classifier self-tests using disposable unittests, no product mutations
"""#1066 actual unittest output must identify the bound assertion to kill."""
import importlib.util
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

sys.dont_write_bytecode = True
spec = importlib.util.spec_from_file_location('reinsert_negative', Path(__file__).with_name('negative_controls.py'))
runner = importlib.util.module_from_spec(spec)
spec.loader.exec_module(runner)
TEST = 'BoundCase.test_bound'
MARKER = '#1066 classifier bound assertion'


class ClassificationTests(unittest.TestCase):
    def output(self, body):
        with tempfile.TemporaryDirectory(prefix='pdrs-1066-classifier-') as temp:
            path = Path(temp) / 'toy.py'
            path.write_text('import unittest\nclass BoundCase(unittest.TestCase):\n'
                            '    def test_bound(self):\n        ' + body + '\n'
                            'unittest.main(verbosity=2)\n')
            return subprocess.run([sys.executable, '-I', '-B', str(path), TEST],
                                  capture_output=True, text=True, timeout=15)

    def test_bound_assertion_is_killed(self):
        result = self.output(f'self.fail({MARKER!r})')
        self.assertEqual(runner.classify(result, TEST, MARKER)[0], 'KILLED',
                         '#1066 classifier must recognize the bound assertion')

    def test_pass_is_survived(self):
        result = self.output('pass')
        self.assertEqual(runner.classify(result, TEST, MARKER)[0], 'SURVIVED',
                         '#1066 classifier must reject a surviving mutation')

    def test_different_assertion_is_invalid(self):
        result = self.output("self.fail('different assertion')")
        self.assertEqual(runner.classify(result, TEST, MARKER)[0], 'INVALID',
                         '#1066 classifier must require its bound failure point')

    def test_echoed_marker_is_invalid(self):
        result = self.output(f"print({MARKER!r}); self.fail('different assertion')")
        self.assertEqual(runner.classify(result, TEST, MARKER)[0], 'INVALID',
                         '#1066 stdout cannot supply assertion evidence')

    def test_error_is_invalid(self):
        result = self.output(f'raise RuntimeError({MARKER!r})')
        self.assertEqual(runner.classify(result, TEST, MARKER)[0], 'INVALID',
                         '#1066 harness errors cannot kill a mutation')

    def test_skip_is_invalid(self):
        result = self.output("self.skipTest('unavailable')")
        self.assertEqual(runner.classify(result, TEST, MARKER)[0], 'INVALID',
                         '#1066 skipped positive tests cannot pass')

    def test_wrong_named_success_is_invalid(self):
        result = self.output('pass')
        self.assertEqual(runner.classify(result, 'BoundCase.test_other', MARKER)[0], 'INVALID',
                         '#1066 positive must run the exact bound test')

    def test_stale_anchor_raises(self):
        with self.assertRaisesRegex(ValueError, '#1066 stale anchor'):
            runner.changed('pass\n', 'missing', 'False', 'toy.py')

    def test_uncompilable_mutant_raises(self):
        with self.assertRaises(SyntaxError, msg='#1066 mutant must compile before execution'):
            runner.changed('pass\n', 'pass', 'if', 'toy.py')


if __name__ == '__main__':
    unittest.main(verbosity=2)
