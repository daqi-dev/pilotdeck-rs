#!/usr/bin/env python3
"""Guard against counting dependency failures as killed product mutations."""
import importlib.util
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

SCRIPT = Path(__file__).with_name('negative-518.py')
spec = importlib.util.spec_from_file_location('negative518', SCRIPT)
runner = importlib.util.module_from_spec(spec)
spec.loader.exec_module(runner)


class MutationEvidenceTests(unittest.TestCase):
    def test_missing_baseline_rejects_before_writes(self):
        original = {path: path.read_bytes() for path in (runner.PRODUCT, runner.ORACLE)}
        env = os.environ.copy()
        env.pop('PDRS_BASELINE', None)
        # No cargo is available: the guard must run before launching any command.
        env['PATH'] = ''
        with tempfile.TemporaryDirectory() as tmp:
            evidence = Path(tmp) / 'evidence'
            proc = subprocess.run([sys.executable, str(SCRIPT), str(evidence)],
                                  env=env, capture_output=True, text=True)
            self.assertEqual(proc.returncode, 2, proc.stdout + proc.stderr)
            self.assertIn('INVALID: set PDRS_BASELINE', proc.stderr)
            self.assertNotIn('CONFIRMED', proc.stdout + proc.stderr)
            self.assertFalse(evidence.exists())
        self.assertEqual(original, {path: path.read_bytes() for path in original})

    def test_dependency_panic_is_invalid_even_when_target_failed(self):
        tests = ['replacement_fixture_regenerates_identically']
        failed = f'test {runner.PREFIX}{tests[0]} ... FAILED\n'
        dependency_failure = failed + 'set PDRS_BASELINE to run this ignored test'
        self.assertIsNotNone(runner.invalid_reason('corrupt_regenerated_observation',
                                                  tests, 101, dependency_failure))
        actual_mismatch = failed + 'replacement/oracle.json differs from the regenerated /tmp/oracle.json'
        self.assertIsNone(runner.invalid_reason('corrupt_regenerated_observation',
                                               tests, 101, actual_mismatch))

    def test_each_mutation_requires_its_own_failure_diagnostics(self):
        for name, _, _, _, _, _, tests, _ in runner.MUTATIONS:
            with self.subTest(mutation=name):
                failed = '\n'.join(f'test {runner.PREFIX}{test} ... FAILED' for test in tests)
                self.assertIsNotNone(runner.invalid_reason(name, tests, 101, failed))
                diagnostic = '\n'.join(runner.FAILURE_TEXT[name])
                self.assertIsNone(runner.invalid_reason(name, tests, 101, failed + '\n' + diagnostic))
                self.assertIsNotNone(runner.invalid_reason(name, tests, 0, failed + '\n' + diagnostic))
                self.assertIsNotNone(runner.invalid_reason(name, tests, 101,
                                                          failed + '\n' + diagnostic + '\ncould not compile'))


if __name__ == '__main__':
    unittest.main()
