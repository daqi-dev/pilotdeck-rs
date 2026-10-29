# anchor-selfcheck: unsupported synthetic unittest results and private Python mirrors, no in-place product mutation table
"""#701: kill-judge forgeries, inventory gaps, and private mirror boundaries."""
import os
import sys

# #1023 fix-r2: identify the directory by (device, inode), not by spelling.
_boot_dir = os.path.dirname(os.path.realpath(__file__))
_boot_roots = (_boot_dir, os.path.dirname(_boot_dir))
if not sys.flags.isolated or any(
        os.path.isdir(entry or os.curdir)
        and any(os.path.samefile(entry or os.curdir, root) for root in _boot_roots)
        for entry in sys.path):
    raise SystemExit(f'#701: unsafe import path; run with -I by file: python3 -I {__file__}')

import importlib.util
import ast
import io
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch

path = Path(__file__).resolve().with_name('negative_controls.py')
spec = importlib.util.spec_from_file_location('isolation_negative_controls', path)
driver = importlib.util.module_from_spec(spec)
sys.modules[spec.name] = driver
exec(compile(path.read_bytes(), str(path), 'exec'), driver.__dict__)
apparatus_tests = driver.load('test_cli_parity', driver.DEFAULT_APPARATUS / 'test_cli_parity.py')
MARKER = '#701: exclusive synthetic assertion'


def outcome(action, *, name='bound', subtest=False):
    class Bound(unittest.TestCase):
        def runTest(self):
            if subtest:
                with self.subTest(probe='bound'):
                    action(self)
            else:
                action(self)

        def id(self):
            return name

    stream = io.StringIO()
    result = unittest.TextTestRunner(stream=stream, resultclass=driver.AssertionResult).run(Bound())
    return result, stream.getvalue()


class JudgeTests(unittest.TestCase):
    def test_only_bound_assertion_identity_earns_a_kill(self):
        for subtest in (False, True):
            with self.subTest(subtest=subtest):
                result, _ = outcome(lambda test: test.fail(MARKER), subtest=subtest)
                self.assertEqual(driver.classify(result, 'bound', MARKER)[0], 'KILLED',
                                 '#701: actual bound AssertionError must kill')
        for action, name in ((lambda test: test.fail('unrelated'), 'bound'),
                             (lambda test: test.fail(MARKER), 'other'),
                             (lambda test: test.skipTest(MARKER), 'bound'),
                             (lambda test: exec('raise RuntimeError(MARKER)'), 'bound')):
            result, _ = outcome(action, name=name)
            self.assertEqual(driver.classify(result, 'bound', MARKER)[0], 'INVALID',
                             '#701: wrong identity/message, skip or error cannot kill')
        result, _ = outcome(lambda test: None)
        self.assertEqual(driver.classify(result, 'bound', MARKER)[0], 'SURVIVED',
                         '#701: passing mutant must survive')
        result.testsRun = 0
        self.assertEqual(driver.classify(result, 'bound', MARKER)[0], 'INVALID',
                         '#701: zero tests cannot kill')

    def test_log_echo_cannot_earn_a_kill(self):
        result, _ = outcome(lambda test: test.fail('unrelated'))
        result.stream.write(MARKER)
        self.assertEqual(driver.classify(result, 'bound', MARKER)[0], 'INVALID',
                         '#701: stream echoes must not replace exception identity')

    def test_skipped_positive_is_not_a_pass(self):
        result, _ = outcome(lambda test: test.skipTest(MARKER))
        self.assertFalse(driver.passed(result), '#701: skipped positive/restored test is invalid')

    def test_restore_failure_invalidates_kill_and_later_rows_still_run(self):
        good, _ = outcome(lambda test: None)
        bad, _ = outcome(lambda test: test.fail('restore failed'))
        killed, _ = outcome(lambda test: test.fail(MARKER), name='test_cli_parity.BootstrapTests.bound')
        source = driver.DEFAULT_APPARATUS / 'README.md'
        entries = [(name, source, source.read_text() + '\n', 'bound', MARKER)
                   for name in ('first', 'second')]
        answers = [(result, '') for result in (good, killed, bad, good, killed, good)]
        rows = []
        with patch.object(driver, 'bootstrap_regressions', return_value=iter(entries)), \
                patch.object(driver, 'run_bound_test', side_effect=answers):
            driver.bootstrap_checks(driver.DEFAULT_APPARATUS, rows, None, None)
        self.assertEqual([row['status'] for row in rows], ['INVALID', 'KILLED'],
                         '#701: failed restore invalidates only its row; later rows must run')


class DocumentTests(unittest.TestCase):
    def check_document(self, content, expected, *, inventory='scripts/cli-parity/README.md\n'):
        method = 'test_documented_commands_use_isolated_mode'
        full_name = 'test_cli_parity.BootstrapTests.' + method
        command_result = subprocess.CompletedProcess([], 0 if inventory else 1, inventory, '')
        with patch.object(apparatus_tests.subprocess, 'run', return_value=command_result):
            result, log = driver.run_bound_test(
                apparatus_tests, method, driver.DEFAULT_APPARATUS / 'README.md', content)
        if expected is None:
            self.assertTrue(driver.passed(result), '#701: valid documented shape must pass: ' + log)
        else:
            status, reason = driver.classify(result, full_name, expected)
            print(log, end='', file=sys.stderr, flush=True)
            self.assertEqual(status, 'KILLED', '#701: document mutation must reach bound assertion: ' + reason)

    def test_document_shapes_and_matching_negative_controls(self):
        for command in (
                'python -I scripts/cli-parity/run_round.py',
                'python3 -I scripts/cli-parity/run_round.py',
                'python3.13 -I scripts/cli-parity/run_round.py',
                'python3.13t -I scripts/cli-parity/run_round.py',
                '/usr/bin/python3 -I scripts/cli-parity/run_round.py',
                '/python3 -I scripts/cli-parity/run_round.py',
                './venv/bin/python -I scripts/cli-parity/run_round.py',
                '"/opt/python bin/python3.13" -I scripts/cli-parity/run_round.py',
                "'/opt/python bin/python3.13' -I scripts/cli-parity/run_round.py",
                'python3 -I "$PWD/scripts/cli-parity/run_round.py"',
                "python3 -I 'scripts/cli-parity/run_round.py'",
                '${PY} -I "${ROOT}/scripts/cli-parity/run_round.py"',
                '"python3.13" -I scripts/cli-parity/run_round.py',
                '"$PY" -I scripts/cli-parity/run_round.py',
                'python3 -W ignore -X dev -I scripts/cli-parity/run_round.py',
                'python3 -I \\\n scripts/cli-parity/run_round.py'):
            with self.subTest(command=command):
                self.check_document(command, None)
                self.check_document(command.replace(' -I', '', 1),
                                    '#701: scripts/cli-parity/README.md:1: CLI parity command requires -I')
                self.check_document(command, None)

    def test_empty_inventory_and_missing_readme_cannot_pass(self):
        for inventory in ('', 'scripts/cli-parity/README.md\n'):
            self.check_document('no runnable commands',
                                '#701: documented command inventory must not be empty', inventory=inventory)
        self.check_document('no runnable commands',
                            '#701: documented command inventory must include scripts/cli-parity/README.md',
                            inventory='src/pilotdeck-cli/tests/fixtures/chat-search/README.md\n')

    def test_documented_discover_is_rejected(self):
        self.check_document('python3 -I -m unittest discover -s scripts/cli-parity',
                            '#701: scripts/cli-parity/README.md:1: CLI parity command requires an explicit file')

    def test_isolation_must_be_an_option_not_an_argument(self):
        for option in ('-W', '-X'):
            self.check_document(f'python3 {option} -I scripts/cli-parity/run_round.py',
                                '#701: scripts/cli-parity/README.md:1: CLI parity command requires -I')
        for option in ('-m unittest discover -s', '-c', '--', 'other.py'):
            self.check_document(f'python3 -I {option} scripts/cli-parity/run_round.py',
                                '#701: scripts/cli-parity/README.md:1: CLI parity command requires an explicit file')


class MirrorTests(unittest.TestCase):
    def test_package_compile_timeout_keeps_remaining_rows(self):
        rows = []
        with tempfile.TemporaryDirectory(prefix='pdrs-701-package-timeout-') as temp:
            holder = Path(temp)
            mirror = holder / 'apparatus'
            mirror.mkdir()
            with patch.object(driver.subprocess, 'run',
                              side_effect=subprocess.TimeoutExpired('synthetic compile', 60)):
                driver.package_checks(mirror, rows, holder)
        self.assertEqual([row['name'] for row in rows],
                         [f'imposter/{name}-package' for name, _ in driver.IMPOSTERS],
                         '#701: compile timeout must not skip later package rows')
        self.assertTrue(all(row['status'] == 'INVALID' for row in rows),
                        '#701: compile timeouts cannot kill')

    def test_fixture_guard_rejects_imposters_before_stdlib(self):
        with tempfile.TemporaryDirectory(prefix='pdrs-701-fixture-') as temp:
            mirror = Path(temp)
            (mirror / 'json.py').write_text('raise RuntimeError("#701 FIXTURE IMPOSTER EXECUTED")\n')
            (mirror / 'pathlib.py').write_text('raise RuntimeError("#701 FIXTURE IMPOSTER EXECUTED")\n')
            for filename in ('negative_controls.py', 'test_negative_controls.py'):
                source = Path(__file__).with_name(filename).read_text()
                for mutated in (False, True, False):
                    candidate = source.replace('if not sys.flags.isolated or', 'if False and', 1) if mutated else source
                    compile(candidate, filename, 'exec')
                    entry = mirror / filename
                    entry.write_text(candidate)
                    result = subprocess.run([sys.executable, str(entry), '-h'],
                                            capture_output=True, text=True, timeout=30)
                    self.assertEqual(result.returncode, 1, '#701: fixture probe must exit at its guard or imposter')
                    expected = 'FIXTURE IMPOSTER EXECUTED' if mutated else '#701: unsafe import path'
                    self.assertIn(expected, result.stderr, '#701: fixture guard/imposter witness must be reached')

    def test_environment_writers_only_receive_private_mirror(self):
        seen = []
        def witness(apparatus, rows, holder):
            self.assertNotEqual(apparatus, driver.DEFAULT_APPARATUS,
                                '#701: environment controls must not receive checkout directory')
            self.assertTrue(apparatus.is_relative_to(holder), '#701: mirror must be privately owned')
            self.assertEqual({p.name for p in apparatus.iterdir()}, set(driver.MIRROR_FILES),
                             '#701: mirror must copy the single shared inventory')
            seen.append(apparatus)
            driver.record(rows, 'synthetic-mirror', 'KILLED', 'private mirror verified')
        with patch.object(sys, 'argv', ['negative_controls.py']), \
                patch.object(driver, 'bootstrap_checks'), patch.object(driver, 'static_checks'), \
                patch.object(driver, 'no_isolated_checks', new=witness), \
                patch.object(driver, 'package_checks', new=witness), \
                patch.object(driver, 'extension_check', new=witness):
            self.assertEqual(driver.main(), 0, '#701: private mirror execution must finish')
        self.assertEqual(len(seen), 3, '#701: all environment writers must run')
        self.assertTrue(all(not path.exists() for path in seen), '#701: owned mirrors must be cleaned')

    def test_real_directory_regression_is_killed_by_mirror_test(self):
        source = Path(driver.__file__).read_text()
        tree = ast.parse(source)
        main_node = next(node for node in tree.body if isinstance(node, ast.FunctionDef) and node.name == 'main')
        original = ast.get_source_segment(source, main_node)
        changed = original.replace('check(mirror, rows, holder)', 'check(apparatus, rows, holder)', 1)
        self.assertNotEqual(changed, original, '#701: mirror mutation anchor must exist')
        namespace = dict(driver.__dict__)
        exec(compile(changed, driver.__file__, 'exec'), namespace)
        test = 'MirrorTests.test_environment_writers_only_receive_private_mirror'
        # The mutated main has a separate globals dictionary. Writer mocks are
        # installed by the bound test, so refresh that dictionary on invocation.
        def changed_main():
            namespace.update({name: driver.__dict__[name] for name in (
                'bootstrap_checks', 'static_checks', 'no_isolated_checks', 'package_checks', 'extension_check')})
            return namespace['main']()
        stream = io.StringIO()
        with patch.object(driver, 'main', changed_main):
            result = unittest.TextTestRunner(stream=stream, resultclass=driver.AssertionResult).run(
                unittest.TestSuite([MirrorTests('test_environment_writers_only_receive_private_mirror')]))
        # main records the boundary assertion as INVALID and returns failure;
        # the bound test's exclusive completion assertion is the expected kill.
        self.assertEqual(driver.classify(result, __name__ + '.' + test,
                                          '#701: private mirror execution must finish')[0], 'KILLED',
                         '#701: real-directory mutation must fail the bound mirror test: ' + stream.getvalue())
        print(stream.getvalue(), end='', file=sys.stderr, flush=True)


if __name__ == '__main__':
    unittest.main(verbosity=2)
