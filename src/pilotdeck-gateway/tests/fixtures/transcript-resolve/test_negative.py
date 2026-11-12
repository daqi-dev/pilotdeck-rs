# anchor-selfcheck: unsupported unit tests of the transcript-resolve kill judge, no product source mutations
#!/usr/bin/env python3
"""#1087 fix-r3 controls for the transcript-resolve kill judge.

The delivered judge (``negative_controls.py::classify``) is the shared strict
#813 criterion: the named test's own ``FAILED`` status line plus the bound
assertion's message inside that test's own panic payload, with a compile
failure explicitly excluded. The controls below pin the positive shape and
the false-kill shapes the old whole-log substring check
(``f'test {test} ... FAILED' in log`` plus ``assertion in log``) accepted --
a sibling test's panic, an assertion only in captured stdout, and an
assertion inside build output. Every log is a synthetic libtest transcript;
no cargo run happens here (the real-sample kill is proven by the
coordinator's full run).

Run with:

    python3 -B transcript-resolve/test_negative.py
"""
import importlib.util
import sys
from pathlib import Path
import unittest


def load_runner():
    source = Path(__file__).with_name('negative_controls.py')
    spec = importlib.util.spec_from_file_location('transcript_resolve_negative',
                                                  source)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)  # main() only runs under __main__
    return module


class TranscriptResolveJudgeControls(unittest.TestCase):
    """The transcript-resolve kill judge must only accept the named test's panic."""

    @classmethod
    def setUpClass(cls):
        cls.runner = load_runner()

    @staticmethod
    def _panic_log(test, payload, *, status='FAILED', thread=None):
        counts = ('0 passed; 1 failed' if status == 'FAILED' else '1 passed; 0 failed')
        summary = f'test result: {"FAILED" if status == "FAILED" else "ok"}. ' \
                  f'{counts}; 0 ignored; 0 measured; 0 filtered out'
        return (
            '\nrunning 1 test\n'
            f'test {test} ... {status}\n'
            '\nfailures:\n'
            f"thread '{thread or test}' (2841) panicked at "
            'tests/transcript_resolve.rs:12:5:\n'
            f'{payload}\n'
            'note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace\n'
            f'{summary}\n'
        )

    def judge(self, log, returncode, test, assertion):
        return self.runner.classify(returncode, log, test, assertion)

    TEST = 'relative_base_deleted_cwd'
    ASSERTION = 'transcript resolve matrix: relative-deleted'

    def test_named_test_assertion_panic_kills(self):
        """The positive shape, across the representative bound assertions."""
        # Named `samples`, not `cases`: anchor_selfcheck's exemption-signal
        # scan reads any store of a table name as a mutation-table
        # declaration, and this file is exempt with no tables.
        samples = (
            ('relative_base_deleted_cwd', 'transcript resolve matrix: relative-deleted'),
            ('relative_base_normal_cwd', 'transcript resolve matrix: join-relative-normal'),
            ('relative_base_deleted_cwd', 'list propagates cwd failure'),
            ('relative_base_deleted_cwd', 'rewind propagates cwd failure'),
        )
        for test, assertion in samples:
            with self.subTest(assertion=assertion):
                log = self._panic_log(test, f'{assertion} (bound assertion fired)')
                self.assertTrue(self.judge(log, 101, test, assertion),
                                "#1087 the named test's own assertion panic must kill")

    def test_captured_stdout_is_not_a_kill(self):
        """An assertion echoed by the test's own stdout is not a kill."""
        log = (
            '\nrunning 1 test\n'
            f'test {self.TEST} ... FAILED\n'
            '\nfailures:\n'
            f'---- {self.TEST} stdout ----\n'
            f'adapter dump for debugging: {self.ASSERTION}\n'
            f"thread '{self.TEST}' (2841) panicked at "
            'tests/transcript_resolve.rs:44:9:\n'
            'assertion `left == right` failed\n'
            'note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace\n'
            'test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out\n'
        )
        self.assertTrue(not self.judge(log, 101, self.TEST, self.ASSERTION),
                        '#1087 an assertion in captured stdout is not a kill')

    def test_sibling_panic_is_not_a_kill(self):
        """An assertion in a sibling test's panic is not a kill."""
        sibling = 'absolute_base_deleted_cwd'
        log = (
            '\nrunning 2 tests\n'
            f'test {self.TEST} ... ok\n'
            f'test {sibling} ... FAILED\n'
            '\nfailures:\n'
            f"thread '{sibling}' (2841) panicked at "
            'tests/transcript_resolve.rs:44:9:\n'
            f'{self.ASSERTION}\n'
            'note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace\n'
            'test result: FAILED. 1 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out\n'
        )
        self.assertTrue(not self.judge(log, 101, self.TEST, self.ASSERTION),
                        "#1087 a sibling test's panic is not a kill")

    def test_build_output_assertion_is_not_a_kill(self):
        """An assertion inside build output is never a kill."""
        log = (
            f'error[E0308]: mismatched types at `{self.ASSERTION}`\n'
            ' --> tests/transcript_resolve.rs:12:5\n'
            'error: could not compile `pilotdeck-gateway` '
            '(test "transcript_resolve") due to 1 previous error\n'
        )
        self.assertTrue(not self.judge(log, 101, self.TEST, self.ASSERTION),
                        '#1087 an assertion inside build output is not a kill')

    def test_green_run_is_not_a_kill(self):
        """A passing bound test is never a kill."""
        log = self._panic_log(self.TEST, 'no failure', status='ok')
        self.assertTrue(not self.judge(log, 0, self.TEST, self.ASSERTION),
                        '#1087 a green bound test is not a kill')

    def test_cargo_pins_the_named_test_with_exact(self):
        """The libtest filter stays exact: one test per process."""
        source = Path(__file__).with_name('negative_controls.py').read_text(
            encoding='utf-8')
        self.assertIn("'--', '--exact'", source,
                      '#1087 the cargo invocation must keep --exact so the '
                      'named test is the only test in the process')

    def test_mutation_write_is_one_atomic_rename(self):
        """#1087 fix-r4 (sibling #5): the mutation publish is one rename.

        A truncating ``write_text`` killed mid-write-back leaves a half-
        written source the next preflight refuses; the shared publish
        helper keeps the single-rename contract, and the restore bytes are
        the guard's own exit verification, not a local re-assert.
        """
        source = Path(__file__).with_name('negative_controls.py').read_text(
            encoding='utf-8')
        self.assertIn('write_source_atomic(root, source, changed.encode())',
                      source,
                      '#1087 the mutation publish must go through the '
                      'shared single-rename helper')
        self.assertNotIn('assert source.read_bytes() == original', source,
                         '#1087 the restore bytes are verified by '
                         'mutating_sources on exit, not re-asserted locally')


if __name__ == '__main__':
    unittest.main()
