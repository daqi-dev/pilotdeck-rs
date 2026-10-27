"""Kill-judgment unit tests on synthetic interleaved cargo logs; no cargo.

The runner is loaded read-only and no product source is mutated here. The
logs are synthetic libtest pretty output, including the interleaving the
`failures:` list judgment must survive and the coincidences it must refuse.
"""
import importlib.util
from pathlib import Path
import subprocess
import unittest

spec = importlib.util.spec_from_file_location(
    'metadata_from_env_negative', Path(__file__).with_name('negative_controls.py'))
runner = importlib.util.module_from_spec(spec)
spec.loader.exec_module(runner)

PREFIX = runner.PREFIX
MARKER = runner.MUTATIONS[0][5]  # 'metadata from_env matrix:' of the cwd-none row
CELLS = sorted({test for _name, _path, _old, _new, tests, _marker in runner.MUTATIONS
                for test in tests})


def libtest_log(failed, interleave=False, extra_stdout=''):
    """Synthesize pretty-format output; a receipt can split name and verdict."""
    lines = [f'running {len(CELLS)} tests']
    for test in CELLS:
        verdict = 'ok' if test not in failed else 'FAILED'
        if interleave:
            # Any stdout line landing between the test name and its verdict
            # (here: a receipt printed before the verdict) breaks the adjacent
            # `... FAILED` spelling that the old judgment required.
            lines.append(f'test {PREFIX}{test} ... PDRS_VERIFIED_CHILD {runner.case_name(test)}')
            lines.append(verdict)
        else:
            lines.append(f'test {PREFIX}{test} ... {verdict}')
    if failed:
        lines += ['', 'failures:']
        lines += [f'    {PREFIX}{test}' for test in CELLS if test in failed]
    lines += ['', f'test result: {"FAILED" if failed else "ok"}. '
               f'{len(CELLS) - len(failed)} passed; {len(failed)} failed; 2 ignored;']
    return '\n'.join(lines) + '\n' + extra_stdout


def panic_text(test, marker, message=None):
    return (f"thread '{PREFIX}{test}' panicked at regression.rs:83:5:\n"
            f'{message or f"{marker} {runner.case_name(test)}"}\n'
            'note: run with `RUST_BACKTRACE=1` for a backtrace\n')


class KillJudgment(unittest.TestCase):
    def test_interleaved_receipt_still_kills(self):
        # The receipt of a passing child splits the test name from its FAILED
        # verdict; the failures list plus the dedicated message, not the
        # adjacency of `... FAILED`, must carry the kill.
        failed = runner.DELETED
        stdout = libtest_log(failed, interleave=True)
        stderr = ''.join(panic_text(test, MARKER) for test in failed)
        result = subprocess.CompletedProcess([], 101, stdout, stderr)
        log = stdout + stderr
        for test in failed:
            self.assertNotIn(f'{PREFIX}{test} ... FAILED', log,
                             'sample must genuinely break the old adjacency')
        self.assertTrue(runner.killed(result, log, failed, MARKER))

    def test_foreign_panic_with_the_same_text_is_not_a_kill(self):
        # Another cell panics carrying the declared cells' exact messages, and
        # the log even echoes the adjacent `... FAILED` spelling for them (an
        # output construction coincidence). The declared cells are not in the
        # failures list, so the kill is refused; the old adjacent judgment,
        # which only looked for those two spellings, would have accepted it.
        foreign = 'tilde_normal_cwd'
        declared = runner.DELETED
        marker = MARKER
        stdout = libtest_log([foreign], extra_stdout=''.join(
            f'echo: test {PREFIX}{test} ... FAILED\n' for test in declared))
        stderr = panic_text(foreign, marker, '; '.join(
            f'{marker} {runner.case_name(test)}' for test in declared))
        result = subprocess.CompletedProcess([], 101, stdout, stderr)
        log = stdout + stderr
        for test in declared:
            self.assertIn(f'{PREFIX}{test} ... FAILED', log)
            self.assertIn(f'{marker} {runner.case_name(test)}', log)
        self.assertFalse(runner.killed(result, log, declared, marker))
        reason = runner.failure_reason(result, log, declared, marker)
        self.assertIn('stayed green', reason)

    def test_declared_failure_without_dedicated_message_is_not_a_kill(self):
        failed = runner.DELETED
        marker = MARKER
        stdout = libtest_log(failed)
        stderr = ''.join(panic_text(test, marker,
                                    'assertion `left == right` failed')
                         for test in failed)
        result = subprocess.CompletedProcess([], 101, stdout, stderr)
        log = stdout + stderr
        self.assertFalse(runner.killed(result, log, failed, marker))
        reason = runner.failure_reason(result, log, failed, marker)
        self.assertIn('without the dedicated marker', reason)

    def test_positive_and_compile_failure_do_not_count(self):
        green = subprocess.CompletedProcess([], 0, libtest_log([], interleave=True), '')
        self.assertTrue(runner.passed(green, green.stdout))
        broken = subprocess.CompletedProcess([], 101, '', 'error[E0308]: mismatched types')
        self.assertFalse(runner.killed(broken, broken.stderr, runner.DELETED,
                                         MARKER))


if __name__ == '__main__':
    unittest.main(verbosity=2)
