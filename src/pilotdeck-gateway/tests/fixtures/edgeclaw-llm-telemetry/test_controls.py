# anchor-selfcheck: unsupported runner unit tests using synthetic cargo output and mocked source reads, no product mutation definitions
"""Runner checks use synthetic cargo output; never mutate product sources."""
import importlib.util
from pathlib import Path
import subprocess
import unittest
from unittest.mock import patch

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


if __name__ == '__main__':
    unittest.main()
