"""Harness fault injection only; no cargo or product source mutation here."""
import contextlib
import io
import json
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch

import negative_suite as suite


TEST = 'runtime::fixture::bound_test'
ANCHOR = 'bound assertion message'


def output(kind):
    if kind == 'pass':
        return subprocess.CompletedProcess([], 0,
            f'test {TEST} ... ok\ntest result: ok. 1 passed; 0 failed;\n', '')
    if kind == 'kill':
        return subprocess.CompletedProcess([], 101,
            f'test {TEST} ... FAILED\ntest result: FAILED\n',
            f'panicked at fixture.rs:1: {ANCHOR}\n')
    raise ValueError(kind)


class FailureListTests(unittest.TestCase):
    """The shared matrix judgment reads libtest's failures list, not adjacency."""

    def test_failures_list_reads_names_despite_interleaved_output(self):
        cell = 'runtime::fixture::relative_deleted_cwd'
        log = ('running 1 test\n'
               'test runtime::fixture::relative_deleted_cwd ... PDRS_VERIFIED_CHILD relative-deleted\n'
               'FAILED\n'
               '\nfailures:\n'
               f'    {cell}\n'
               'test result: FAILED. 0 passed; 1 failed;\n')
        self.assertEqual(suite.failed_tests(log, 'runtime::fixture::'), {cell})
        self.assertNotIn(f'test {cell} ... FAILED', log,
                         'sample must genuinely break the old adjacency')

    def test_case_name_dashes_cwd_cells(self):
        self.assertEqual(suite.case_name('relative_deleted_cwd'), 'relative-deleted')
        self.assertEqual(suite.case_name('missing_home_normal_cwd'), 'missing-home-normal')

    def test_cell_hits_split_marker_and_foreign_failures(self):
        marker = 'matrix message'
        cell, foreign = 'relative_deleted_cwd', 'tilde_deleted_cwd'
        listed = '\nfailures:\n    runtime::fixture::relative_deleted_cwd\n'
        # A declared failure with its dedicated message is the only hit.
        self.assertEqual(
            suite.cell_hits(listed + f'{marker} {suite.case_name(cell)}\n',
                            'runtime::fixture::', [cell], marker, suite.case_name),
            ([cell], []))
        # A declared failure without that message is unrelated, never a kill.
        self.assertEqual(
            suite.cell_hits(listed, 'runtime::fixture::', [cell], marker, suite.case_name),
            ([], [cell]))
        # A foreign failure quoting the declared message, plus the adjacent
        # `... FAILED` spelling as an echo, is still not a hit.
        coincidence = ('\nfailures:\n    runtime::fixture::tilde_deleted_cwd\n'
                       f'test runtime::fixture::{cell} ... FAILED\n'
                       f'{marker} {suite.case_name(cell)}\n')
        self.assertEqual(
            suite.cell_hits(coincidence, 'runtime::fixture::', [cell], marker, suite.case_name),
            ([], []))


class SuiteTests(unittest.TestCase):
    def exercise(self, first, *, positive=None, restored=None, bad_anchor=False, guard=None,
                 guard_exit=None, restore_failure=False):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = root / 'fixture.txt'
            source.write_bytes(b'original')
            out = root / 'logs'
            out.mkdir()
            cases = [suite.Case(name, source, 'absent' if bad_anchor and name == 'first' else 'original',
                                name, TEST, ANCHOR) for name in ('first', 'second')]
            calls = []
            write_bytes = Path.write_bytes

            def restore(path, data):
                if restore_failure and path == source and path.read_text() == 'first':
                    raise OSError('injected restoration failure')
                return write_bytes(path, data)

            def leave_guard(*args):
                write_bytes(source, b'original')
                if guard_exit:
                    raise guard_exit
                return False

            def run(command, **kwargs):
                self.assertEqual(command[-5:], [TEST, '--', '--exact', '--include-ignored', '--nocapture'],
                                 'bound ignored tests must execute')
                calls.append(source.read_text())
                if source.read_text() == 'first':
                    if isinstance(first, Exception):
                        raise first
                    return first
                if source.read_text() == 'second':
                    return output('kill')
                if len(calls) == 1 and positive:
                    return positive
                if len(calls) == (2 if bad_anchor or positive else 3) and restored:
                    return restored
                return output('pass')

            def mutate(text, scope, replacements):
                old, new = replacements[0]
                if text.count(old) != 1:
                    raise AssertionError('missing anchor')
                return text.replace(old, new)

            with patch.object(suite, 'mutating_sources') as sources, \
                    patch.object(suite, 'wait_for_slot', return_value=output('pass')), \
                    patch.object(suite, 'run_owned', side_effect=run), \
                    patch.object(Path, 'write_bytes', new=restore), \
                    contextlib.redirect_stdout(io.StringIO()) as stdout:
                sources.return_value.__enter__.return_value = {source: b'original'}
                sources.return_value.__enter__.side_effect = guard
                sources.return_value.__exit__.side_effect = leave_guard
                code = suite.run_cases(root, cases, out, {'CARGO_TARGET_DIR': str(root / 'target')}, mutate)
            self.assertEqual(source.read_bytes(), b'original', 'each injected fault restores bytes')
            summary = json.loads((out / 'summary.json').read_text())
            self.assertEqual([r['mutation'] for r in summary['controls']], ['first', 'second'],
                             'all selected controls must be accounted for')
            if guard is None and not restore_failure:
                expected = 'INVALID' if guard_exit else 'KILLED'
                self.assertEqual(summary['controls'][1]['status'], expected, 'successor must run')
                self.assertEqual(calls[-3:], ['original', 'second', 'original'])
            if restore_failure:
                self.assertNotIn('second', calls, 'unsafe restoration must stop later mutations')
            return code, summary, stdout.getvalue()

    def test_all_killed_exits_zero(self):
        code, summary, text = self.exercise(output('kill'))
        self.assertEqual(code, 0)
        self.assertEqual(summary['counts'], {'KILLED': 2, 'SURVIVED': 0, 'INVALID': 0})
        self.assertIn('KILLED=2', text)

    def test_survivor_does_not_abort_successor(self):
        code, summary, _ = self.exercise(output('pass'))
        self.assertEqual(code, 1, 'survivor must not count as killed')
        self.assertEqual(summary['controls'][0]['status'], 'SURVIVED', 'survivor must not count as killed')

    def test_invalid_failures_never_count_as_kills_and_continue(self):
        failures = {
            'compile': subprocess.CompletedProcess([], 101, '', 'error[E0001]: compilation failed'),
            'wrong-test': subprocess.CompletedProcess([], 101, 'test other ... FAILED\ntest result: FAILED',
                                                       f'panicked at other.rs: {ANCHOR}'),
            'wrong-anchor': subprocess.CompletedProcess([], 101,
                f'test {TEST} ... FAILED\ntest result: FAILED', 'panicked at: unrelated assertion'),
            'zero-tests': subprocess.CompletedProcess([], 0, 'test result: ok. 0 passed; 0 failed;', ''),
            'timeout': subprocess.TimeoutExpired(['cargo'], 0.01, output=output('kill').stdout,
                                                 stderr=output('kill').stderr),
            'spawn-error': OSError('injected spawn failure'),
        }
        for name, result in failures.items():
            with self.subTest(name=name):
                code, summary, _ = self.exercise(result)
                self.assertEqual(code, 1)
                self.assertEqual(summary['controls'][0]['status'], 'INVALID')

    def test_bad_positive_and_restoration_continue(self):
        for phase in ('positive', 'restored'):
            with self.subTest(phase=phase):
                code, summary, _ = self.exercise(output('kill'), **{phase: output('kill')})
                self.assertEqual(code, 1)
                self.assertEqual(summary['controls'][0]['status'], 'INVALID')
                self.assertIn(phase, summary['controls'][0]['reason'])

    def test_bad_anchor_does_not_abort_successor(self):
        code, summary, _ = self.exercise(output('kill'), bad_anchor=True)
        self.assertEqual(code, 1)
        self.assertIn('anchor: AssertionError: missing anchor', summary['controls'][0]['reason'])

    def test_guard_refusal_accounts_for_every_case(self):
        code, summary, _ = self.exercise(output('kill'), guard=RuntimeError('baseline differs'))
        self.assertEqual(code, 1)
        self.assertEqual(summary['counts'], {'KILLED': 0, 'SURVIVED': 0, 'INVALID': 2})

    def test_guard_exit_invalidates_previous_kills(self):
        code, summary, _ = self.exercise(output('kill'), guard_exit=RuntimeError('guard restore failed'))
        self.assertEqual(code, 1, 'guard restoration failure must exit nonzero')
        self.assertEqual(summary['counts'], {'KILLED': 0, 'SURVIVED': 0, 'INVALID': 2})
        self.assertTrue(all(row['status'] == 'INVALID' for row in summary['controls']),
                        'guard restoration failure invalidates every previous kill')

    def test_byte_restore_failure_stops_unsafe_mutation(self):
        code, summary, _ = self.exercise(output('kill'), restore_failure=True)
        self.assertEqual(code, 1)
        self.assertEqual(summary['counts'], {'KILLED': 0, 'SURVIVED': 0, 'INVALID': 2})
        self.assertIn('injected restoration failure', summary['controls'][0]['reason'])

    def test_each_failure_marker_is_required(self):
        case = suite.Case('test', None, '', '', TEST, ANCHOR)
        killed = output('kill')
        for marker in ('test result: FAILED', f'test {TEST} ... FAILED', 'panicked at', ANCHOR):
            with self.subTest(marker=marker):
                weakened = subprocess.CompletedProcess([], 101, killed.stdout.replace(marker, ''),
                                                        killed.stderr.replace(marker, ''))
                self.assertEqual(suite.classify(weakened, case)[0], 'INVALID',
                                 'every bound runtime marker is required')
        killed.returncode = 0
        self.assertEqual(suite.classify(killed, case)[0], 'INVALID')

    def test_positive_needs_exact_test_and_one_pass(self):
        passed = output('pass')
        for marker in (f'test {TEST} ... ok', 'test result: ok. 1 passed;'):
            with self.subTest(marker=marker):
                self.assertFalse(suite.passed(subprocess.CompletedProcess([], 0,
                                  passed.stdout.replace(marker, ''), ''), TEST),
                                 'positive needs exact test and one pass')


if __name__ == '__main__':
    unittest.main(verbosity=2)
