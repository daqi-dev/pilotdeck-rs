#!/usr/bin/env python3
# anchor-selfcheck: unsupported unit tests of the mutation runner
"""Guard against counting dependency failures as killed product mutations."""
import importlib.util
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

SCRIPT = Path(__file__).with_name('negative-518.py')
spec = importlib.util.spec_from_file_location('negative518', SCRIPT)
runner = importlib.util.module_from_spec(spec)
spec.loader.exec_module(runner)


def panic(test, text):
    """A libtest panic block owned by the full test name."""
    return f"thread '{runner.PREFIX}{test}' panicked at fixture.rs:1:1:\n{text}\n"


def failed_line(test):
    return f'test {runner.PREFIX}{test} ... FAILED\n'


WARNING_MUTATION = 'remove_warning_boundary'
WARNING_TESTS = next(row[6] for row in runner.MUTATIONS if row[0] == WARNING_MUTATION)
WARNING_COMMIT = 'warning panic skipped commit cleanup'
WARNING_TIMEOUT = 'warning panic stranded timeout phase'
WARNING_GETTER = 'hasActive failed'


def warning_log(commit=WARNING_COMMIT, timeout=WARNING_TIMEOUT,
                getter=WARNING_GETTER, *, stdout=''):
    """The recorded three-test shape: commit, timeout, then getter."""
    status = ''.join(failed_line(test) for test in WARNING_TESTS)
    blocks = ''.join(panic(test, text) for test, text in
                     zip(WARNING_TESTS, (commit, timeout, getter)))
    return status + stdout + blocks + 'test result: FAILED. 0 passed; 3 failed;\n'


def warning_reason(**kwargs):
    return runner.invalid_reason(WARNING_MUTATION, WARNING_TESTS, 101,
                                 warning_log(**kwargs))


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
        failed = failed_line(tests[0])
        dependency_failure = failed + 'set PDRS_BASELINE to run this ignored test'
        self.assertIsNotNone(runner.invalid_reason('corrupt_regenerated_observation',
                                                  tests, 101, dependency_failure))
        actual_mismatch = (failed + panic(
            tests[0], 'replacement/oracle.json differs from the regenerated /tmp/oracle.json'))
        self.assertIsNone(runner.invalid_reason('corrupt_regenerated_observation',
                                               tests, 101, actual_mismatch))

    def test_sibling_panic_with_the_same_text_is_rejected(self):
        # #819 defect channel: the marker is in another test's panic block.
        tests = ['replacement_fixture_regenerates_identically']
        raw = (failed_line(tests[0])
               + "thread 'other::test' panicked at fixture.rs:2:2:\n"
               + 'replacement/oracle.json differs from the regenerated /tmp/oracle.json\n')
        self.assertIsNotNone(runner.invalid_reason('corrupt_regenerated_observation',
                                                  tests, 101, raw))

    def test_each_mutation_requires_its_own_failure_diagnostics(self):
        for name, _, _, _, _, _, tests, _ in runner.MUTATIONS:
            with self.subTest(mutation=name):
                failed = '\n'.join(failed_line(test) for test in tests)
                texts = runner.FAILURE_TEXT[name]
                bound = {test: text
                         for text, test in runner.FAILURE_TARGETS.get(name, {}).items()}
                blocks = '\n'.join(
                    panic(test, bound.get(
                        test, texts[index] if len(texts) == len(tests) else texts[0]))
                    for index, test in enumerate(tests))
                self.assertIsNotNone(runner.invalid_reason(name, tests, 101, failed))
                self.assertIsNone(runner.invalid_reason(name, tests, 101, failed + '\n' + blocks))
                self.assertIsNotNone(runner.invalid_reason(name, tests, 0, failed + '\n' + blocks))
                self.assertIsNotNone(runner.invalid_reason(name, tests, 101,
                                                          failed + '\n' + blocks + '\ncould not compile'))

    def test_each_required_warning_diagnostic_is_individually_required(self):
        # #818 fix-r4: remove_warning_boundary has three target tests but two
        # required diagnostics. Either one missing must be INVALID, never
        # CONFIRMED because its sibling matched.
        self.assertIsNone(warning_reason())
        for missing, text in (('commit', WARNING_COMMIT), ('timeout', WARNING_TIMEOUT)):
            with self.subTest(missing=missing):
                reason = warning_reason(**{missing: 'unrelated dependency failure'})
                self.assertIsNotNone(reason, 'one matching diagnostic must not confirm')
                self.assertIn(text, reason)

    def test_warning_diagnostic_is_bound_to_its_target_test(self):
        # Both texts are present, so all(any(...)) alone would confirm. The
        # explicit target binding must reject a diagnostic on the wrong test.
        reason = warning_reason(commit=WARNING_TIMEOUT, timeout=WARNING_COMMIT)
        self.assertIsNotNone(reason, 'wrong-test diagnostics must be INVALID')
        self.assertIn(f'{runner.PREFIX}{WARNING_TESTS[0]} panic block lacks {WARNING_COMMIT!r}',
                      reason)
        self.assertIn(f'{runner.PREFIX}{WARNING_TESTS[1]} panic block lacks {WARNING_TIMEOUT!r}',
                      reason)

    def test_shared_diagnostic_list_requires_every_text(self):
        # No target binding here: this isolates the all(any(...)) check that
        # the review's double-any weakened (#818 fix-r4).
        shared_tests = ['shared_a', 'shared_b', 'shared_c']
        raw = (''.join(failed_line(test) for test in shared_tests)
               + panic(shared_tests[0], 'first required')
               + panic(shared_tests[1], 'unrelated dependency failure')
               + panic(shared_tests[2], 'other unrelated failure')
               + 'test result: FAILED. 0 passed; 3 failed;\n')
        with patch.dict(runner.FAILURE_TEXT, {'synthetic': ['first required', 'second required']}), \
                patch.dict(runner.FAILURE_TARGETS, {'synthetic': {}}):
            reason = runner.invalid_reason('synthetic', shared_tests, 101, raw)
            self.assertIsNotNone(reason, 'one shared diagnostic must not confirm')
            self.assertIn('second required', reason,
                          'every required text must be present, not any one')
            both = raw.replace('unrelated dependency failure', 'second required')
            self.assertIsNone(runner.invalid_reason('synthetic', shared_tests, 101, both))

    def test_warning_diagnostic_only_in_stdout_is_invalid(self):
        reason = warning_reason(timeout='unrelated dependency failure',
                                stdout=WARNING_TIMEOUT + '\n')
        self.assertIsNotNone(reason, 'stdout is not a named panic payload')


if __name__ == '__main__':
    unittest.main()
