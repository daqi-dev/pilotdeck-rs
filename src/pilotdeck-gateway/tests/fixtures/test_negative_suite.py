# anchor-selfcheck: unsupported unit tests of the shared mutation suite runner
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


class AnchorPreflightTests(unittest.TestCase):
    def test_scope_drift_and_duplicate_fail_without_skipping_successor(self):
        import sys
        sys.path.insert(0, str(Path(__file__).resolve().parent / 'turn-runner'))
        from _negative_common import check_mutations
        # Mirror the #741k failure: a clone changes one anchor and a new
        # select duplicates another. Neither may be accepted or hide later rows.
        original = ("    fn factory() { recorder.clone(); biased; biased; }\n"
                    "    fn client() { recorder.clone(); }\n")
        with tempfile.TemporaryDirectory() as directory:
            source = Path(directory) / 'fixture.rs'
            source.write_text(original)
            rows = [
                ('stale', source, 'factory', [('recorder;', 'bad;')]),
                ('duplicate', source, 'factory', [('biased;', 'bad;')]),
                ('noop', source, 'factory', [('recorder.clone();', 'recorder.clone();')]),
                ('successor', source, 'factory', [('recorder.clone();', 'bad;')]),
            ]
            with contextlib.redirect_stdout(io.StringIO()) as output:
                code = check_mutations(rows)
            summary = json.loads(output.getvalue())
            self.assertEqual(code, 1, '#741k drift must fail preflight')
            self.assertEqual([row['status'] for row in summary['controls']],
                             ['INVALID', 'INVALID', 'INVALID', 'CHECKED'],
                             '#741k invalid anchors must not skip the successor')
            self.assertEqual(source.read_text(), original, '#741k preflight is read-only')
            with contextlib.redirect_stdout(io.StringIO()):
                self.assertEqual(check_mutations(rows[-1:]), 0,
                                 '#741k function-scoped unique anchor must pass')
                self.assertEqual(check_mutations([]), 1, '#741k empty preflight must fail')

class ReceiptTests(unittest.TestCase):
    PREFIX = 'metadata::from_env_tests::'
    CELL = 'relative_deleted_cwd'
    ASSERTION = 'metadata from_env matrix: relative-deleted'

    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.out = Path(self.temp.name)
        self.targets = [(self.CELL, self.ASSERTION)]
        self.key = (self.PREFIX + self.CELL, self.ASSERTION)
        self.run = suite.ReceiptRun(self.out, 'current', [self.key])
        self.result = subprocess.CompletedProcess([], 101,
            f'test {self.key[0]} ... FAILED\n\nfailures:\n    {self.key[0]}\n\n'
            'test result: FAILED. 0 passed; 1 failed; 0 ignored;\n', '')

    def judge(self, result=None, targets=None, **kwargs):
        return suite.judge(result or self.result, self.PREFIX,
                           self.targets if targets is None else targets, self.run, **kwargs)

    def receipt(self, value=None):
        self.run.paths[self.key].write_text(self.run.nonce if value is None else value)

    def test_named_failure_and_exact_nonce_are_both_required(self):
        self.assertFalse(self.judge()[0])
        self.receipt()
        self.assertTrue(self.judge()[0])
        for code, stdout in ((0, self.result.stdout), (1, self.result.stdout),
                             (-9, self.result.stdout), (101, ''),
                             (101, self.result.stdout.replace(self.key[0], 'other'))):
            self.assertFalse(self.judge(subprocess.CompletedProcess([], code, stdout, ''))[0])
        # stderr and diagnostic substrings cannot substitute for the stdout line.
        self.assertFalse(self.judge(subprocess.CompletedProcess([], 101, '', self.result.stdout))[0])
        self.assertFalse(self.judge(subprocess.CompletedProcess([], 101,
                         'diagnostic ' + self.result.stdout, ''))[0])

    def test_fresh_run_paths_and_nonce_reject_previous_run(self):
        previous = suite.ReceiptRun(self.out, 'previous', [self.key])
        previous.paths[self.key].write_text(previous.nonce)
        self.assertNotEqual(self.run.nonce, previous.nonce)
        self.assertNotEqual(self.run.paths[self.key], previous.paths[self.key])
        self.assertFalse(self.judge()[0])
        self.receipt(previous.nonce)
        self.assertFalse(self.judge()[0])
        with self.assertRaises(FileExistsError):
            suite.ReceiptRun(self.out, 'current', [self.key])

    def test_corrupt_receipts_are_not_kills(self):
        for value in ('', 'old', self.run.nonce[:-1], self.run.nonce + '\n',
                      self.run.nonce + 'x' * 100000):
            self.receipt(value)
            self.assertFalse(self.judge()[0], repr(value[:66]))
        self.run.paths[self.key].write_bytes(b'\xff' * 64)
        self.assertFalse(self.judge()[0])

    def test_nonregular_and_unreadable_receipts_are_not_kills(self):
        import os
        path = self.run.paths[self.key]
        path.mkdir()
        self.assertFalse(self.judge()[0])
        path.rmdir()
        other = self.out / 'other'
        other.write_text(self.run.nonce)
        path.symlink_to(other)
        self.assertFalse(self.judge()[0])
        path.unlink()
        os.mkfifo(path)
        self.assertFalse(self.judge()[0], 'FIFO must not block')
        path.unlink()
        with patch('os.open', side_effect=PermissionError('denied')):
            self.assertFalse(self.judge()[0])

    def test_test_and_assertion_identity_are_both_required(self):
        self.receipt()
        for targets in ([], [(self.CELL, 'other assertion')],
                        [('other_cell', self.ASSERTION)], self.targets * 2,
                        self.targets + [(self.CELL, 'other assertion')]):
            self.assertFalse(self.judge(targets=targets)[0])
            self.assertFalse(self.judge(targets=targets, any_cell=True)[0])

    def test_all_and_explicit_any_cell(self):
        second = ('sibling', 'sibling assertion')
        targets = self.targets + [second]
        self.receipt()
        self.assertFalse(self.judge(targets=targets)[0])
        self.assertTrue(self.judge(targets=targets, any_cell=True)[0])
        sibling_failed = subprocess.CompletedProcess([], 101, self.result.stdout +
            f'test {self.PREFIX}sibling ... FAILED\n', '')
        self.assertFalse(self.judge(sibling_failed, targets=targets, any_cell=True)[0],
                         'any_cell never accepts an unrelated declared failure')
        self.run.paths[self.key].unlink()
        self.assertFalse(self.judge(targets=targets, any_cell=True)[0])

    def test_empty_and_duplicate_receipt_targets_are_refused(self):
        for index, targets in enumerate(([], [self.key, self.key],
                                         [self.key, (self.key[0], 'other assertion')])):
            with self.assertRaises(ValueError):
                suite.ReceiptRun(self.out, f'bad-{index}', targets)

    def test_environment_clears_spoofed_parent_and_overrides_old_config(self):
        env = self.run.environment({self.run.ENV: 'old', self.run.PARENT_ENV: 'sibling', 'HOME': 'kept'})
        self.assertNotIn(self.run.PARENT_ENV, env)
        self.assertEqual(json.loads(env[self.run.ENV]), self.run.config)
        self.assertEqual(env['HOME'], 'kept')

    def test_issue889_r2_r3_r4_replays_never_kill_without_current_receipt(self):
        # Rewritten from review-r{2,3,4}/codex.log, including both echo toggles.
        # Real failure remains setup unwrap; only the diagnostic prose changes.
        worker = self.PREFIX + 'environment_worker'
        setup = 'called Result::unwrap() on an Err value: PermissionDenied'
        old_panic = (f"thread '{worker}' panicked at regression.rs:64:5:\n"
                     f'assertion `left == right` failed: {self.ASSERTION}\n')
        summary = 'test result: FAILED. 0 passed; 1 failed; 0 ignored;\n\n'
        setup_panic = f"thread '{worker}' panicked at regression.rs:64:5:\n{setup}\n"
        parent = (f"thread '{self.key[0]}' panicked at test_process.rs:32:37:\n"
                  f'{worker}/relative-deleted: expected one passed test and receipt relative-deleted: '
                  'exit status: 101\nrunning 1 test\n')
        diagnostics = {
            'r2-no-echo': setup_panic,
            'r2-ordinary-marker': f'diagnostic: checking {self.ASSERTION}\n' + setup_panic,
            'r3-no-echo': setup_panic,
            'r3-old-stdout-panic': old_panic + summary + setup_panic,
            'r4-conflicting': setup_panic + old_panic,
            'r4-old-summary-after-real': setup_panic + summary + old_panic,
            'r4-old-summary-before-real': summary + old_panic + setup_panic,
        }
        for name, diagnostic in diagnostics.items():
            with self.subTest(name=name):
                result = subprocess.CompletedProcess([], 101, self.result.stdout, parent + diagnostic)
                self.assertFalse(self.judge(result)[0])
                # Echoing even this run's public nonce in either stream is not a file receipt.
                result.stderr += self.run.nonce
                self.assertFalse(self.judge(result)[0])
        # Positive pair: arbitrary prose cannot veto authentic assertion evidence.
        self.receipt()
        self.assertTrue(self.judge()[0])


class MatrixRunnerTests(unittest.TestCase):
    def exercise(self, fault=None):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = root / 'source.rs'
            source.write_text('original')
            rows = [('first', source, 'original', 'first', [('bound', 'assertion')]),
                    ('second', source, 'original', 'second', [('bound', 'assertion')])]
            if fault == 'anchor':
                rows[0] = ('first', source, 'absent', 'first', [('bound', 'assertion')])
            phases = []
            original = b'original'
            guards = []
            restores = []
            write_bytes = Path.write_bytes
            def restore(path, data):
                if fault == 'restore' and path == source and path.read_text() == 'first' and not restores:
                    restores.append(True)
                    raise OSError('injected matrix restore failure')
                return write_bytes(path, data)
            @contextlib.contextmanager
            def guard(*args, **kwargs):
                self.assertNotIn('baseline', kwargs, 'must use HEAD')
                guards.append(True)
                if fault == 'guard-enter' and len(guards) == 1:
                    raise RuntimeError('injected guard entry refusal')
                self.assertEqual(source.read_bytes(), original, 'guard rechecks restored source')
                try:
                    yield {source: original}
                finally:
                    source.write_bytes(original)
                if fault == 'guard-exit' and len(guards) == 1:
                    raise RuntimeError('injected guard exit refusal')
            def run(command, **kwargs):
                current = source.read_text()
                compile = '--no-run' in command
                phases.append((current, compile))
                if compile:
                    return subprocess.CompletedProcess([], int(fault == 'compile' and current == 'first'), '', '')
                if current == 'first' and fault == 'timeout':
                    raise subprocess.TimeoutExpired(command, 900)
                if current == 'original' or (current == 'first' and fault == 'survive'):
                    return subprocess.CompletedProcess([], 0,
                        'test result: ok. 1 passed; 0 failed;\nPDRS_VERIFIED_CHILD bound\n', '')
                config = json.loads(kwargs['env'][suite.ReceiptRun.ENV])
                if current != 'first' or fault != 'missing-receipt':
                    Path(config['targets'][0]['path']).write_text(config['nonce'])
                return subprocess.CompletedProcess([], 101, 'test prefix::bound ... FAILED\n', '')
            with patch.object(suite, 'mutating_sources', side_effect=guard), \
                    patch.object(suite, 'run_owned', side_effect=run), \
                    patch.object(Path, 'write_bytes', new=restore), \
                    patch.object(suite, 'wait_for_slot', return_value=output('pass')), \
                    patch.dict('os.environ', {'CARGO_TARGET_DIR': str(root / 'target')}), \
                    contextlib.redirect_stdout(io.StringIO()):
                code = suite.matrix_main(root, root / 'runner.py', 'prefix::', rows,
                    passed_count=1, child_count=1, paths=[source], args=[str(root / 'out')])
            self.assertEqual(source.read_bytes(), original)
            report = json.loads((root / 'out/summary.json').read_text())
            self.assertEqual(report['controls'][1]['status'], 'KILLED', 'must run successor')
            self.assertEqual(phases[-3:], [('second', True), ('second', False), ('original', False)])
            return code, report, phases

    def test_each_mutant_compiles_and_restores(self):
        code, report, phases = self.exercise()
        self.assertEqual(code, 0)
        self.assertEqual(report['counts']['KILLED'], 2)
        self.assertEqual(phases[:4], [('original', False), ('first', True), ('first', False), ('original', False)])

    def test_faults_continue_and_never_kill(self):
        for fault in ('compile', 'timeout', 'survive', 'missing-receipt',
                      'restore', 'guard-enter', 'guard-exit', 'anchor'):
            with self.subTest(fault=fault):
                code, report, phases = self.exercise(fault)
                self.assertEqual(code, 1)
                self.assertEqual(report['controls'][0]['status'], 'SURVIVED' if fault == 'survive' else 'INVALID')
                if fault == 'compile':
                    self.assertNotIn(('first', False), phases, 'uncompiled mutant must not execute')


if __name__ == '__main__':
    unittest.main(verbosity=2)
