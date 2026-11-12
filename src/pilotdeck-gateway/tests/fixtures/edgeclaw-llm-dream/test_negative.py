# anchor-selfcheck: unsupported unit tests of the replay kill judge, no product source mutations
#!/usr/bin/env python3
"""#1087 fix-r2 controls for the llm-dream kill judge.

The delivered judge (``negative.py::failure_is_bound``) is the shared #813
criterion: the bound test's own ``FAILED`` status line plus the mutant's
``#771 ...`` fragment inside that test's own panic payload. The controls below
pin the positive shape and the false-kill shapes the old log-substring judge
accepted — the loose ``panicked at`` OR (any panic in the log stood in for the
named test's own failure), a sibling test's panic, a fragment only in captured
stdout, and a fragment inside build output. Every log is a synthetic libtest
transcript; no cargo run happens here (the real-sample kill is proven by the
coordinator's full run).

Run with:

    python3 -B edgeclaw-llm-dream/test_negative.py
"""
import sys
from pathlib import Path
import unittest


class LlmDreamJudgeControls(unittest.TestCase):
    """The llm-dream kill judge must only accept the named test's own fragment."""

    def setUp(self):
        entry = str(Path(__file__).resolve().parent)
        if entry not in sys.path:
            sys.path.insert(0, entry)
            self.addCleanup(sys.path.remove, entry)
        import negative
        self.runner = negative

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
            'tests/edgeclaw_llm_dream.rs:12:5:\n'
            f'{payload}\n'
            'note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace\n'
            f'{summary}\n'
        )

    def test_named_test_fragment_panic_kills(self):
        """The positive shape: the named test's own fragment panic."""
        test = 'plan_dream_clusters_matches_ts'
        fragment = '#771 result planDreamClusters#0'
        log = self._panic_log(test, 'cluster plan mismatch: ' + fragment)
        self.assertTrue(self.runner.failure_is_bound(log, fragment, test) is True,
                        "#1087 the named test's own fragment panic must kill")

    def test_captured_stdout_is_not_a_kill(self):
        """A fragment printed to captured stdout is not a kill."""
        test = 'plan_dream_clusters_matches_ts'
        fragment = '#771 result planDreamClusters#0'
        log = (
            '\nrunning 1 test\n'
            f'test {test} ... FAILED\n'
            '\nfailures:\n'
            f'---- {test} stdout ----\n'
            f'oracle dump for debugging: {fragment}\n'
            f"thread '{test}' (2841) panicked at "
            'tests/edgeclaw_llm_dream.rs:44:9:\n'
            'called `Option::unwrap()` on a `None` value\n'
            'note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace\n'
            'test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out\n'
        )
        self.assertTrue(self.runner.failure_is_bound(log, fragment, test) is False,
                        '#1087 a fragment in captured stdout is not a kill')

    def test_foreign_panic_without_named_failure_is_not_a_kill(self):
        """A panic elsewhere with no named FAILED line is not a kill.

        The old judge accepted ``'panicked at' in log`` as a stand-in for the
        named test's own ``FAILED`` line, so any panic anywhere counted.
        """
        test = 'plan_dream_clusters_matches_ts'
        fragment = '#771 result planDreamClusters#0'
        log = (
            '\nrunning 1 test\n'
            f'test {test} ... ok\n'
            "thread '<unnamed>' (2841) panicked at "
            'src/pilotdeck-gateway/src/runtime/memory/edgeclaw/llm/dream.rs:9:5:\n'
            f'worker thread aborted: {fragment}\n'
            'note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace\n'
            'test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out\n'
        )
        self.assertTrue(self.runner.failure_is_bound(log, fragment, test) is False,
                        '#1087 a panic without the named test failing is not a kill')

    def test_sibling_panic_is_not_a_kill(self):
        """A fragment in a sibling test's panic is not a kill."""
        test = 'plan_dream_clusters_matches_ts'
        fragment = '#771 result planDreamClusters#0'
        log = (
            '\nrunning 2 tests\n'
            f'test {test} ... ok\n'
            'test rewrite_user_profile_matches_ts ... FAILED\n'
            '\nfailures:\n'
            "thread 'rewrite_user_profile_matches_ts' (2841) panicked at "
            'tests/edgeclaw_llm_dream.rs:44:9:\n'
            f'profile rewrite mismatch: {fragment}\n'
            'note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace\n'
            'test result: FAILED. 1 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out\n'
        )
        self.assertTrue(self.runner.failure_is_bound(log, fragment, test) is False,
                        "#1087 a sibling test's panic is not a kill")

    def test_build_output_fragment_is_not_a_kill(self):
        """A fragment inside build output is never a kill."""
        test = 'plan_dream_clusters_matches_ts'
        fragment = '#771 result planDreamClusters#0'
        log = (
            f'error[E0308]: mismatched types in `{fragment}`\n'
            ' --> tests/edgeclaw_llm_dream.rs:12:5\n'
            'error: could not compile `pilotdeck-gateway` '
            '(test "edgeclaw_llm_dream") due to 1 previous error\n'
        )
        self.assertTrue(self.runner.failure_is_bound(log, fragment, test) is False,
                        '#1087 a fragment inside build output is never a kill')


if __name__ == '__main__':
    unittest.main()
