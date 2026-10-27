"""Kill-judgment unit tests on synthetic interleaved cargo logs; no cargo.

The runner is loaded read-only and no product source is mutated here. The
logs are synthetic libtest pretty output, including the interleaving the
`failures:` list judgment must survive and the coincidences it must refuse.
"""
import importlib.util
from pathlib import Path
import subprocess
import sys
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_suite import case_name

spec = importlib.util.spec_from_file_location(
    'metadata_from_env_negative', Path(__file__).with_name('negative_controls.py'))
runner = importlib.util.module_from_spec(spec)
spec.loader.exec_module(runner)

PREFIX = runner.PREFIX
WORKER = runner.WORKER
MARKER = runner.MUTATIONS[0][5]  # 'metadata from_env matrix:' of the cwd-none row
CELLS = sorted({test for _name, _path, _old, _new, tests, _marker in runner.MUTATIONS
                for test in tests})
FOREIGN = 'tilde_normal_cwd'


def libtest_log(failed, interleave=False, extra_stdout=''):
    """Synthesize pretty-format output; a receipt can split name and verdict."""
    lines = [f'running {len(CELLS)} tests']
    for test in CELLS:
        verdict = 'ok' if test not in failed else 'FAILED'
        if interleave:
            # Any stdout line landing between the test name and its verdict
            # (here: a receipt printed before the verdict) breaks the adjacent
            # `... FAILED` spelling that the old judgment required.
            lines.append(f'test {PREFIX}{test} ... PDRS_VERIFIED_CHILD {case_name(test)}')
            lines.append(verdict)
        else:
            lines.append(f'test {PREFIX}{test} ... {verdict}')
    if failed:
        lines += ['', 'failures:', '', 'failures:']
        lines += [f'    {PREFIX}{test}' for test in CELLS if test in failed]
    lines += ['', f'test result: {"FAILED" if failed else "ok"}. '
               f'{len(CELLS) - len(failed)} passed; {len(failed)} failed; 2 ignored;']
    return '\n'.join(lines) + '\n' + extra_stdout


def panic_text(test, marker, message=None):
    return (f"thread '{PREFIX}{test}' (35085042) panicked at regression.rs:83:5:\n"
            f'{message or f"{marker} {case_name(test)}"}\n'
            'note: run with `RUST_BACKTRACE=1` for a backtrace\n')


def parent_failure(test, marker):
    """The real kill shape: the parent's panic embeds the worker's whole output.

    Mirrors the host log (test_process.rs:32 panic embedding the worker's
    libtest run, its nested ``failures:`` list and its own panic).
    """
    case = case_name(test)
    message = (f'{PREFIX}{WORKER}/{case}: expected one passed test and receipt '
               f'{case}: exit status: 101\n\nrunning 1 test\n'
               f'test {PREFIX}{WORKER} ... FAILED\n\nfailures:\n\nfailures:\n'
               f'    {PREFIX}{WORKER}\n\ntest result: FAILED. 0 passed; 1 failed; '
               f'0 ignored;\n\n'
               f"thread '{PREFIX}{WORKER}' (35085063) panicked at regression.rs:83:5:\n"
               f'assertion `left == right` failed: {marker} {case}\n'
               'note: run with `RUST_BACKTRACE=1` for a backtrace\n')
    return panic_text(test, marker, message)


class KillJudgment(unittest.TestCase):
    def judge(self, stdout, stderr, tests, marker, **kwargs):
        return runner.judge(subprocess.CompletedProcess([], 101, stdout, stderr),
                             PREFIX, tests, marker, WORKER, **kwargs)

    def test_interleaved_receipt_still_kills(self):
        # The receipt of a passing child splits the test name from its FAILED
        # verdict; the failures list plus the dedicated message, not the
        # adjacency of `... FAILED`, must carry the kill.
        failed = runner.DELETED
        stdout = libtest_log(failed, interleave=True)
        stderr = ''.join(parent_failure(test, MARKER) for test in failed)
        log = stdout + stderr
        for test in failed:
            self.assertNotIn(f'{PREFIX}{test} ... FAILED', log,
                             'sample must genuinely break the old adjacency')
        killed, reason, hits = self.judge(stdout, stderr, failed, MARKER)
        self.assertTrue(killed, reason)
        self.assertEqual(hits, failed)

    def test_foreign_panic_with_the_same_text_is_not_a_kill(self):
        # Another cell panics carrying the declared cells' exact messages, and
        # the log even echoes the adjacent `... FAILED` spelling for them (an
        # output construction coincidence). The declared cells are not in the
        # failures list, so the kill is refused; a judgment that only looked
        # for those two spellings would have accepted it.
        declared = runner.DELETED
        stdout = libtest_log([FOREIGN], extra_stdout=''.join(
            f'echo: test {PREFIX}{test} ... FAILED\n' for test in declared))
        stderr = panic_text(FOREIGN, MARKER, '; '.join(
            f'{MARKER} {case_name(test)}' for test in declared))
        log = stdout + stderr
        for test in declared:
            self.assertIn(f'{PREFIX}{test} ... FAILED', log)
            self.assertIn(f'{MARKER} {case_name(test)}', log)
        killed, reason, _ = self.judge(stdout, stderr, declared, MARKER)
        self.assertFalse(killed)
        self.assertIn('stayed green', reason)

    def test_foreign_panic_echoing_indented_declared_cells_is_not_a_kill(self):
        # Composite scenario #889 A: every declared cell passes and only a
        # foreign test fails, but its panic echoes the declared cells'
        # indented failures-list names and their dedicated messages.
        declared = runner.DELETED
        echo = '\n'.join(f'    {PREFIX}{test}\n{MARKER} {case_name(test)}'
                         for test in declared)
        stdout = libtest_log([FOREIGN])
        stderr = panic_text(FOREIGN, MARKER, echo)
        log = stdout + stderr
        for test in declared:
            self.assertIn(f'    {PREFIX}{test}', log,
                          'sample must genuinely echo the indented cell name')
            self.assertIn(f'{MARKER} {case_name(test)}', log)
        killed, reason, _ = self.judge(stdout, stderr, declared, MARKER)
        self.assertFalse(killed)
        self.assertIn('stayed green', reason)

    def test_declared_setup_failure_with_foreign_markers_is_not_a_kill(self):
        # Composite scenario #889 B: the declared cells are in the outer
        # failures list, but only because their setup failed; every dedicated
        # message comes from another test's panic.
        declared = runner.DELETED
        stdout = libtest_log([*declared, FOREIGN])
        stderr = ''.join(panic_text(test, MARKER, 'setup failed: Permission denied')
                         for test in declared)
        stderr += panic_text(FOREIGN, MARKER, '; '.join(
            f'{MARKER} {case_name(test)}' for test in declared))
        log = stdout + stderr
        for test in declared:
            self.assertIn(f'{MARKER} {case_name(test)}', log,
                          'sample must carry the dedicated message somewhere')
        killed, reason, _ = self.judge(stdout, stderr, declared, MARKER)
        self.assertFalse(killed)
        self.assertIn('without the dedicated marker', reason)

    def test_declared_failure_without_dedicated_message_is_not_a_kill(self):
        failed = runner.DELETED
        stdout = libtest_log(failed)
        stderr = ''.join(panic_text(test, MARKER,
                                    'assertion `left == right` failed')
                         for test in failed)
        killed, reason, _ = self.judge(stdout, stderr, failed, MARKER)
        self.assertFalse(killed)
        self.assertIn('without the dedicated marker', reason)

    def test_positive_and_compile_failure_do_not_count(self):
        green = subprocess.CompletedProcess([], 0, libtest_log([], interleave=True), '')
        self.assertTrue(runner.passed(green, green.stdout))
        broken = subprocess.CompletedProcess([], 101, '', 'error[E0308]: mismatched types')
        killed, reason, _ = runner.judge(broken, PREFIX, runner.DELETED, MARKER, WORKER)
        self.assertFalse(killed)
        self.assertIn('compile failure', reason)


if __name__ == '__main__':
    unittest.main(verbosity=2)
