# anchor-selfcheck: unsupported unit tests of the replay kill judge, no product source mutations
#!/usr/bin/env python3
"""#1087 fix-r2 controls for the dream-runner kill judge.

The delivered judge (``negative.py::failure_is_bound``) is the shared #813
criterion: the bound test's own ``FAILED`` status line plus the mutant's
``#773 ...`` fragment inside that test's own panic payload. The controls below
pin the positive shape and the false-kill shapes the old log-substring judge
accepted — a sibling test's panic, a fragment only in captured stdout, and a
fragment inside build output. Every log is a synthetic libtest transcript; no
cargo run happens here (the real-sample kill is proven by the coordinator's
full run).

Run with:

    python3 -B edgeclaw-dream-runner/test_negative.py
"""
import sys
from pathlib import Path
import unittest


class DreamRunnerJudgeControls(unittest.TestCase):
    """The dream-runner kill judge must only accept the named test's own fragment."""

    def setUp(self):
        entry = str(Path(__file__).resolve().parent)
        if entry not in sys.path:
            sys.path.insert(0, entry)
            self.addCleanup(sys.path.remove, entry)
        import negative
        self.runner = negative

    @staticmethod
    def _panic_log(test, payload, *, status='FAILED', summary=None):
        if summary is None:
            counts = ('0 passed; 1 failed' if status == 'FAILED' else '1 passed; 0 failed')
            summary = f'test result: {"FAILED" if status == "FAILED" else "ok"}. ' \
                      f'{counts}; 0 ignored; 0 measured; 0 filtered out'
        return (
            '\nrunning 1 test\n'
            f'test {test} ... {status}\n'
            '\nfailures:\n'
            f"thread '{test}' (2841) panicked at "
            'tests/edgeclaw_dream_runner.rs:12:5:\n'
            f'{payload}\n'
            'note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace\n'
            f'{summary}\n'
        )

    def test_named_test_fragment_panic_kills(self):
        """The positive shape: the named test's own fragment panic."""
        test = 'run_scratch_rejects_deletes'
        fragment = '#773 remove refused: the run parent must not be removed'
        log = self._panic_log(test, 'scratch delete rejected: ' + fragment)
        self.assertTrue(self.runner.failure_is_bound(log, fragment, test) is True,
                        "#1087 the named test's own fragment panic must kill")

    def test_captured_stdout_is_not_a_kill(self):
        """A fragment printed to captured stdout is not a kill."""
        test = 'run_scratch_rejects_deletes'
        fragment = '#773 remove refused: the run parent must not be removed'
        log = (
            '\nrunning 1 test\n'
            f'test {test} ... FAILED\n'
            '\nfailures:\n'
            f'---- {test} stdout ----\n'
            f'scratch dump for debugging: {fragment}\n'
            f"thread '{test}' (2841) panicked at "
            'tests/edgeclaw_dream_runner.rs:44:9:\n'
            'called `Option::unwrap()` on a `None` value\n'
            'note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace\n'
            'test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out\n'
        )
        self.assertTrue(self.runner.failure_is_bound(log, fragment, test) is False,
                        '#1087 a fragment in captured stdout is not a kill')

    def test_sibling_panic_is_not_a_kill(self):
        """A fragment in a sibling test's panic is not a kill."""
        test = 'run_scratch_rejects_deletes'
        fragment = '#773 remove refused: the run parent must not be removed'
        log = (
            '\nrunning 2 tests\n'
            f'test {test} ... ok\n'
            'test observe_uses_stage_paths ... FAILED\n'
            '\nfailures:\n'
            "thread 'observe_uses_stage_paths' (2841) panicked at "
            'tests/edgeclaw_dream_runner.rs:44:9:\n'
            f'observe path wrong: {fragment}\n'
            'note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace\n'
            'test result: FAILED. 1 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out\n'
        )
        self.assertTrue(self.runner.failure_is_bound(log, fragment, test) is False,
                        "#1087 a sibling test's panic is not a kill")

    def test_build_output_fragment_is_not_a_kill(self):
        """A fragment inside build output is never a kill."""
        test = 'run_scratch_rejects_deletes'
        fragment = '#773 remove refused: the run parent must not be removed'
        log = (
            f'error[E0308]: mismatched types in `{fragment}`\n'
            ' --> tests/edgeclaw_dream_runner.rs:12:5\n'
            'error: could not compile `pilotdeck-gateway` '
            '(test "edgeclaw_dream_runner") due to 1 previous error\n'
        )
        self.assertTrue(self.runner.failure_is_bound(log, fragment, test) is False,
                        '#1087 a fragment inside build output is never a kill')

    def test_aborted_run_without_summary_is_not_a_kill(self):
        """#1087 fix-r4 (sibling #2): a partial transcript is not a kill.

        The bound test already printed its FAILED line and its own payload
        (the ``--nocapture`` abort shape measured on a real libtest process
        killed by SIGKILL), but the run never reached libtest's own
        ``test result:`` summary. An aborted run is not a kill decision.
        """
        test = 'run_scratch_rejects_deletes'
        fragment = '#773 remove refused: the run parent must not be removed'
        log = self._panic_log(test, 'scratch delete rejected: ' + fragment, summary='')
        self.assertTrue(self.runner.failure_is_bound(log, fragment, test) is False,
                        '#1087 an aborted run without the FAILED summary line is not a kill')


if __name__ == '__main__':
    unittest.main()
