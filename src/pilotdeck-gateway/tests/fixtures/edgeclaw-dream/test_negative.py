# anchor-selfcheck: unsupported unit tests of the replay kill judge, no product source mutations
#!/usr/bin/env python3
"""#1087 fix-r1 controls for the dream replay kill judge.

The delivered judge (``negative.py::assertion_failure``) is the shared #813
criterion: the bound test's own ``FAILED`` status line plus one of the oracle's
``{group}/`` anchors inside that test's own panic payload. The controls below
pin the positive shape and the false-kill shapes the old log-substring judge
accepted — a sibling test's panic, an anchor only in captured stdout, a bare
``assert_eq!`` line without the group anchor, and an anchor inside build
output. Every log is a synthetic libtest transcript; no cargo run happens
here (the real-sample kill is proven by the coordinator's full run).

Run with:

    python3 -B edgeclaw-dream/test_negative.py
"""
import sys
from pathlib import Path
import unittest


class DreamJudgeControls(unittest.TestCase):
    """The dream kill judge must only accept the named test's own anchor."""

    def setUp(self):
        entry = str(Path(__file__).resolve().parent)
        if entry not in sys.path:
            sys.path.insert(0, entry)
            self.addCleanup(sys.path.remove, entry)
        import negative
        self.runner = negative

    @staticmethod
    def _panic_log(test, payload, *, status='FAILED', summary=None):
        """One synthetic libtest transcript around a single named test."""
        if summary is None:
            counts = ('0 passed; 1 failed' if status == 'FAILED'
                      else '1 passed; 0 failed')
            summary = f'test result: {"FAILED" if status == "FAILED" else "ok"}. ' \
                      f'{counts}; 0 ignored; 0 measured; 0 filtered out'
        return (
            '\nrunning 1 test\n'
            f'test {test} ... {status}\n'
            '\nfailures:\n'
            f"thread '{test}' (2841) panicked at "
            'tests/edgeclaw_dream_helpers.rs:12:5:\n'
            f'{payload}\n'
            'note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace\n'
            f'{summary}\n'
        )

    def test_named_test_anchor_panic_kills(self):
        """The positive shape: the named test's own anchor panic."""
        test = 'mutation_matches_ts'
        log = self._panic_log(test, 'mutation/ applyMutation case=3: expected 2, got 3')
        self.assertTrue(self.runner.assertion_failure(log, test) is True,
                        "#1087 the named test's own anchor panic must kill")

    def test_sibling_panic_is_not_a_kill(self):
        """An anchor in a sibling test's panic is not a kill."""
        test = 'mutation_matches_ts'
        log = (
            '\nrunning 2 tests\n'
            f'test {test} ... ok\n'
            'test clusters_matches_ts ... FAILED\n'
            '\nfailures:\n'
            "thread 'clusters_matches_ts' (2841) panicked at "
            'tests/edgeclaw_dream_helpers.rs:44:9:\n'
            'clusters/ buildClusters case=1: expected 2, got 3\n'
            'note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace\n'
            'test result: FAILED. 1 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out\n'
        )
        self.assertTrue(self.runner.assertion_failure(log, test) is False,
                        "#1087 a sibling test's panic is not a kill")

    def test_captured_stdout_is_not_a_kill(self):
        """An anchor printed to captured stdout is not a kill."""
        test = 'mutation_matches_ts'
        log = (
            '\nrunning 1 test\n'
            f'test {test} ... FAILED\n'
            '\nfailures:\n'
            f'---- {test} stdout ----\n'
            'mutation/ replay dump for debugging\n'
            f"thread '{test}' (2841) panicked at "
            'tests/edgeclaw_dream_helpers.rs:44:9:\n'
            'called `Option::unwrap()` on a `None` value\n'
            'note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace\n'
            'test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out\n'
        )
        self.assertTrue(self.runner.assertion_failure(log, test) is False,
                        '#1087 an anchor in captured stdout is not a kill')

    def test_bare_assert_eq_without_anchor_is_not_a_kill(self):
        """A bare ``assert_eq!`` line without the group anchor is not a kill."""
        test = 'mutation_matches_ts'
        log = self._panic_log(test, 'assertion `left == right` failed\n  left: 2\n right: 3')
        self.assertTrue(self.runner.assertion_failure(log, test) is False,
                        '#1087 a bare assert_eq! line without the group anchor is not a kill')

    def test_build_output_anchor_is_not_a_kill(self):
        """An anchor inside build output is never a kill."""
        test = 'mutation_matches_ts'
        log = (
            'error[E0308]: mismatched types in `mutation/ applyMutation`\n'
            ' --> tests/edgeclaw_dream_helpers.rs:12:5\n'
            'error: could not compile `pilotdeck-gateway` '
            '(test "edgeclaw_dream_helpers") due to 1 previous error\n'
        )
        self.assertTrue(self.runner.assertion_failure(log, test) is False,
                        '#1087 an anchor inside build output is never a kill')


class ShapesJudgeControls(unittest.TestCase):
    """The negative_shapes.py kill judge must only accept the bound case id.

    #1087 fix-r2 (sibling review P1#1): the old whole-log judge accepted a
    ``case=<id>`` substring anywhere in the log, so a sibling test's panic,
    captured stdout, or build output could stand in for the mutant's own
    assertion. The delivered judge routes through negative_panic.
    """

    def setUp(self):
        entry = str(Path(__file__).resolve().parent)
        if entry not in sys.path:
            sys.path.insert(0, entry)
            self.addCleanup(sys.path.remove, entry)
        import negative_shapes
        self.runner = negative_shapes

    @staticmethod
    def _panic_log(test, payload, *, status='FAILED'):
        counts = ('0 passed; 1 failed' if status == 'FAILED' else '1 passed; 0 failed')
        summary = f'test result: {"FAILED" if status == "FAILED" else "ok"}. ' \
                  f'{counts}; 0 ignored; 0 measured; 0 filtered out'
        return (
            '\nrunning 1 test\n'
            f'test {test} ... {status}\n'
            '\nfailures:\n'
            f"thread '{test}' (2841) panicked at "
            'tests/edgeclaw_dream_helpers.rs:12:5:\n'
            f'{payload}\n'
            'note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace\n'
            f'{summary}\n'
        )

    def test_named_test_case_id_panic_kills(self):
        """The positive shape: the named test's own case-id panic."""
        test = 'mutation_matches_ts'
        log = self._panic_log(test, 'mutation/ applyMutation case=applyMutation#3: expected 2, got 3')
        self.assertTrue(self.runner.assertion_failure(log, test, 'applyMutation#3') is True,
                        '#1087 the named test\'s own case-id panic must kill')

    def test_captured_stdout_is_not_a_kill(self):
        """A case id printed to captured stdout is not a kill."""
        test = 'mutation_matches_ts'
        log = (
            '\nrunning 1 test\n'
            f'test {test} ... FAILED\n'
            '\nfailures:\n'
            f'---- {test} stdout ----\n'
            'mutation/ replay dump for debugging case=applyMutation#3\n'
            f"thread '{test}' (2841) panicked at "
            'tests/edgeclaw_dream_helpers.rs:44:9:\n'
            'called `Option::unwrap()` on a `None` value\n'
            'note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace\n'
            'test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out\n'
        )
        self.assertTrue(self.runner.assertion_failure(log, test, 'applyMutation#3') is False,
                        '#1087 a case id in captured stdout is not a kill')

    def test_sibling_panic_is_not_a_kill(self):
        """A case id in a sibling test's panic is not a kill."""
        test = 'mutation_matches_ts'
        log = (
            '\nrunning 2 tests\n'
            f'test {test} ... ok\n'
            'test clusters_matches_ts ... FAILED\n'
            '\nfailures:\n'
            "thread 'clusters_matches_ts' (2841) panicked at "
            'tests/edgeclaw_dream_helpers.rs:44:9:\n'
            'clusters/ buildClusters case=applyMutation#3: expected 2, got 3\n'
            'note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace\n'
            'test result: FAILED. 1 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out\n'
        )
        self.assertTrue(self.runner.assertion_failure(log, test, 'applyMutation#3') is False,
                        "#1087 a sibling test's panic is not a kill")

    def test_bare_assert_eq_without_case_id_is_not_a_kill(self):
        """A bare ``assert_eq!`` diff without the bound case id is not a kill."""
        test = 'mutation_matches_ts'
        log = self._panic_log(test, 'assertion `left == right` failed\n  left: 2\n right: 3')
        self.assertTrue(self.runner.assertion_failure(log, test, 'applyMutation#3') is False,
                        '#1087 a bare assert_eq! diff without the case id is not a kill')


if __name__ == '__main__':
    unittest.main()
