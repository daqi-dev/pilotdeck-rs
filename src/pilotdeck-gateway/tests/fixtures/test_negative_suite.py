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
PREFIX = 'runtime::fixture::'
WORKER = 'environment_worker'
CELLS = ['relative_deleted_cwd', 'tilde_deleted_cwd', 'empty_deleted_cwd']
FOREIGN = 'tilde_normal_cwd'
MARKER = 'matrix message'


def output(kind):
    if kind == 'pass':
        return subprocess.CompletedProcess([], 0,
            f'test {TEST} ... ok\ntest result: ok. 1 passed; 0 failed;\n', '')
    if kind == 'kill':
        return subprocess.CompletedProcess([], 101,
            f'test {TEST} ... FAILED\ntest result: FAILED\n',
            f'panicked at fixture.rs:1: {ANCHOR}\n')
    raise ValueError(kind)


def libtest_stdout(failed, interleave=False, doubled_header=True, extra_stdout=''):
    """Synthesize the outer libtest pretty run; a receipt can split a verdict."""
    lines = [f'running {len(CELLS)} tests']
    for test in CELLS:
        verdict = 'FAILED' if test in failed else 'ok'
        if interleave:
            lines.append(f'test {PREFIX}{test} ... PDRS_VERIFIED_CHILD {suite.case_name(test)}')
            lines.append(verdict)
        else:
            lines.append(f'test {PREFIX}{test} ... {verdict}')
    if failed:
        # Current libtest prints the header twice; older toolchains once.
        lines += ['', 'failures:'] + (['', 'failures:'] if doubled_header else [''])
        lines += [f'    {PREFIX}{test}' for test in CELLS if test in failed]
    lines += ['', f'test result: {"FAILED" if failed else "ok"}. '
               f'{len(CELLS) - len(failed)} passed; {len(failed)} failed; 2 ignored;']
    return '\n'.join(lines) + '\n' + extra_stdout


def panic_text(test, message, thread_id=True):
    line = f"thread '{PREFIX}{test}'" + (" (35084990)" if thread_id else '')
    return (f'{line} panicked at fixture.rs:9:5:\n{message}\n'
            'note: run with `RUST_BACKTRACE=1` for a backtrace\n')


def worker_panic(case, marker=MARKER):
    """The bounded worker's own panic, embedded in its parent's message."""
    return (f"thread '{PREFIX}{WORKER}' (35085019) panicked at fixture.rs:83:5:\n"
            f'assertion `left == right` failed: {marker} {case}\n'
            'note: run with `RUST_BACKTRACE=1` for a backtrace\n')


def parent_failure(test, marker=MARKER):
    """A real kill shape: the parent's message embeds the worker's whole output.

    Mirrors the host log (test_process.rs:32 panic embedding the worker's
    libtest run, its nested ``failures:`` list and its own panic).
    """
    case = suite.case_name(test)
    message = (f'{PREFIX}{WORKER}/{case}: expected one passed test and receipt '
               f'{case}: exit status: 101\n\nrunning 1 test\n'
               f'test {PREFIX}{WORKER} ... FAILED\n\nfailures:\n\nfailures:\n'
               f'    {PREFIX}{WORKER}\n\ntest result: FAILED. 0 passed; 1 failed; '
               f'0 ignored;\n\n{worker_panic(case, marker)}')
    return panic_text(test, message)


def run_result(stdout, stderr, code=101):
    return subprocess.CompletedProcess([], code, stdout, stderr)


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

    def test_failures_list_uses_the_last_header(self):
        # The current toolchain prints the header twice; the names follow the
        # last one, and the outer summary line ends the list.
        stdout = libtest_stdout(CELLS[:1])
        self.assertEqual(suite.failed_tests(stdout, PREFIX), {PREFIX + CELLS[0]})
        single = libtest_stdout(CELLS[:1], doubled_header=False)
        self.assertEqual(suite.failed_tests(single, PREFIX), {PREFIX + CELLS[0]})
        self.assertEqual(suite.failed_tests(libtest_stdout([]), PREFIX), set())

    def test_case_name_dashes_cwd_cells(self):
        self.assertEqual(suite.case_name('relative_deleted_cwd'), 'relative-deleted')
        self.assertEqual(suite.case_name('missing_home_normal_cwd'), 'missing-home-normal')

    def test_cell_hits_split_marker_and_foreign_failures(self):
        cell = CELLS[0]
        # A declared failure whose own panic block carries the dedicated
        # message is the only hit.
        result = run_result(libtest_stdout([cell]), panic_text(cell, f'{MARKER} relative-deleted'))
        self.assertEqual(suite.cell_hits(result, PREFIX, [cell], MARKER, WORKER), ([cell], []))
        # A declared failure without that message is unrelated, never a kill.
        result = run_result(libtest_stdout([cell]), panic_text(cell, 'unrelated panic'))
        self.assertEqual(suite.cell_hits(result, PREFIX, [cell], MARKER, WORKER), ([], [cell]))
        # A foreign failure quoting the declared message, plus the adjacent
        # `... FAILED` spelling as an echo, is still not a hit.
        echo = (f'test {PREFIX}{cell} ... FAILED\n{MARKER} {suite.case_name(cell)}\n')
        result = run_result(libtest_stdout([FOREIGN], extra_stdout=''.join(
            f'echo: test {PREFIX}{test} ... FAILED\n' for test in CELLS[:1])),
            panic_text(FOREIGN, echo))
        log = result.stdout + result.stderr
        self.assertIn(f'{PREFIX}{cell} ... FAILED', log)
        self.assertIn(f'{MARKER} {suite.case_name(cell)}', log)
        self.assertEqual(suite.cell_hits(result, PREFIX, [cell], MARKER, WORKER), ([], []))

    def test_interleaved_receipt_and_embedded_worker_panic_kills(self):
        # The real kill shape: receipts split the verdict spelling, and the
        # dedicated message arrives inside the worker's panic, which is
        # itself embedded in the parent's panic message.
        declared = CELLS[:2]
        stdout = libtest_stdout(declared, interleave=True)
        stderr = ''.join(parent_failure(test) for test in declared)
        result = run_result(stdout, stderr)
        log = stdout + stderr
        for test in declared:
            self.assertNotIn(f'{PREFIX}{test} ... FAILED', log,
                             'sample must genuinely break the old adjacency')
            self.assertNotIn(f'{MARKER} {suite.case_name(test)}', stdout,
                            'the dedicated message only exists on stderr')
        killed, reason, hits = suite.judge(result, PREFIX, declared, MARKER, WORKER)
        self.assertTrue(killed, reason)
        self.assertEqual(hits, declared)

    def test_foreign_panic_echoing_declared_cells_is_not_a_kill(self):
        # Composite scenario #889 A: every declared cell passes and only a
        # foreign test fails, but its multi-line panic echoes the declared
        # cells' indented failures-list names and their dedicated messages.
        declared = CELLS[:2]
        echo = '\n'.join(f'    {PREFIX}{test}\n{MARKER} {suite.case_name(test)}'
                         for test in declared)
        result = run_result(libtest_stdout([FOREIGN]), panic_text(FOREIGN, echo))
        log = result.stdout + result.stderr
        for test in declared:
            self.assertIn(f'    {PREFIX}{test}', log,
                          'sample must genuinely echo the indented cell name')
            self.assertIn(f'{MARKER} {suite.case_name(test)}', log,
                          'sample must genuinely echo the dedicated message')
        self.assertEqual(suite.cell_hits(result, PREFIX, declared, MARKER, WORKER), ([], []))
        killed, reason, _ = suite.judge(result, PREFIX, declared, MARKER, WORKER)
        self.assertFalse(killed, 'an echo inside a foreign panic is not a kill')
        self.assertIn('stayed green', reason)

    def test_declared_setup_failure_with_foreign_markers_is_not_a_kill(self):
        # Composite scenario #889 B: the declared cells are in the outer
        # failures list, but only because their setup failed; every dedicated
        # message comes from another test's panic.
        declared = CELLS[:2]
        setup = ''.join(panic_text(test, 'setup failed: Permission denied')
                        for test in declared)
        stray = panic_text(FOREIGN, '; '.join(
            f'{MARKER} {suite.case_name(test)}' for test in declared))
        result = run_result(libtest_stdout([*declared, FOREIGN]), setup + stray)
        log = result.stdout + result.stderr
        for test in declared:
            self.assertIn(f'{MARKER} {suite.case_name(test)}', log,
                          'sample must carry the dedicated message somewhere')
        self.assertEqual(suite.cell_hits(result, PREFIX, declared, MARKER, WORKER),
                         ([], declared))
        killed, reason, _ = suite.judge(result, PREFIX, declared, MARKER, WORKER)
        self.assertFalse(killed, 'a marker outside the cell\'s own panic is not a kill')
        self.assertIn('without the dedicated marker', reason)

    def test_nested_worker_failures_list_never_counts_as_outer(self):
        # A declared cell's own block embeds the worker's whole libtest run,
        # including the worker's own ``failures:`` list. The kill must stand
        # on the outer list alone, and the worker's name must not leak in.
        declared = CELLS[:2]
        stdout = libtest_stdout(declared)
        stderr = ''.join(parent_failure(test) for test in declared)
        self.assertEqual(suite.failed_tests(stdout, PREFIX),
                         {PREFIX + test for test in declared})
        self.assertNotIn(PREFIX + WORKER, suite.failed_tests(stdout, PREFIX))
        killed, reason, _ = suite.judge(run_result(stdout, stderr),
                                        PREFIX, declared, MARKER, WORKER)
        self.assertTrue(killed, reason)

    def test_worker_panic_inside_a_foreign_block_does_not_leak(self):
        # The worker panic carrying a declared cell's message sits inside a
        # foreign parent's block; the declared cell failed for another reason.
        declared = CELLS[:1]
        stderr = (panic_text(declared[0], 'setup failed: Permission denied')
                  + panic_text(FOREIGN, worker_panic(suite.case_name(declared[0]))))
        result = run_result(libtest_stdout([*declared, FOREIGN]), stderr)
        self.assertIn(f'{MARKER} {suite.case_name(declared[0])}', result.stderr)
        self.assertEqual(suite.cell_hits(result, PREFIX, declared, MARKER, WORKER),
                         ([], declared))

    def test_empty_tests_refuse_to_judge(self):
        # An empty declaration must not satisfy 0 == 0 as a kill (#889 guard).
        result = run_result(libtest_stdout([]), panic_text(FOREIGN, 'unrelated'))
        for call in (lambda: suite.cell_hits(result, PREFIX, [], MARKER, WORKER),
                     lambda: suite.target_hits(result, PREFIX, [], WORKER),
                     lambda: suite.judge(result, PREFIX, [], MARKER, WORKER)):
            with self.assertRaises(ValueError):
                call()

    def test_any_cell_keeps_partial_kill_semantics(self):
        # #879b absolute-guard: one declared cell red with its dedicated
        # message is a kill under any_cell, even while a sibling stays green.
        declared = CELLS[:2]
        stdout = libtest_stdout([declared[0]])
        stderr = parent_failure(declared[0])
        result = run_result(stdout, stderr)
        killed, _, hits = suite.judge(result, PREFIX, declared, MARKER, WORKER, any_cell=True)
        self.assertTrue(killed)
        self.assertEqual(hits, [declared[0]])
        killed, reason, _ = suite.judge(result, PREFIX, declared, MARKER, WORKER)
        self.assertFalse(killed)
        self.assertIn('stayed green', reason)

    def test_panic_blocks_keep_the_embedded_worker_panic_inside(self):
        blocks = suite.panic_blocks(parent_failure(CELLS[0]) + parent_failure(CELLS[1]),
                                    PREFIX, CELLS[0], WORKER)
        self.assertEqual(len(blocks), 1)
        self.assertIn(f'{MARKER} {suite.case_name(CELLS[0])}', blocks[0])
        self.assertIn(f"thread '{PREFIX}{WORKER}'", blocks[0],
                      'the embedded worker panic belongs to the parent block')
        # A panic without the thread id is still a block start.
        blocks = suite.panic_blocks(panic_text(CELLS[0], 'x', thread_id=False), PREFIX,
                                    CELLS[0], WORKER)
        self.assertEqual(len(blocks), 1)


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
