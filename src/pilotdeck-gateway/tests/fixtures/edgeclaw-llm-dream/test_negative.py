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
import os
import sys
import tempfile
from pathlib import Path
import unittest
from unittest.mock import patch


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

    def test_aborted_run_without_summary_is_not_a_kill(self):
        """#1087 fix-r4 (sibling #2): a partial transcript is not a kill.

        The bound test already printed its FAILED line and its own payload
        (the ``--nocapture`` abort shape measured on a real libtest process
        killed by SIGKILL), but the run never reached libtest's own
        ``test result:`` summary. An aborted run is not a kill decision.
        """
        test = 'plan_dream_clusters_matches_ts'
        fragment = '#771 result planDreamClusters#0'
        log = self._panic_log(test, 'cluster plan mismatch: ' + fragment, summary='')
        self.assertTrue(self.runner.failure_is_bound(log, fragment, test) is False,
                        '#1087 an aborted run without the FAILED summary line is not a kill')


class RefusalControls(unittest.TestCase):
    """#1087 fix-r6 (P1b): the stale-anchor refusals must survive a dead stdout.

    ``1>&-`` or a closed stream makes the STALE ANCHOR prints raise; the old
    unguarded prints turned the never-started exit-2 refusal into an
    unhandled OSError crash (rc=1, "ran and was red"). Both refusal sites
    are pinned: the read-only ``--check-anchors`` preflight and the
    in-window stale check before the baseline run. No cargo run happens
    here — the process layer is patched on the imported module.
    """

    def setUp(self):
        entry = str(Path(__file__).resolve().parent)
        if entry not in sys.path:
            sys.path.insert(0, entry)
            self.addCleanup(sys.path.remove, entry)
        import negative
        self.runner = negative
        self.tmp = tempfile.TemporaryDirectory(prefix='llm-dream-refusal-')
        self.addCleanup(self.tmp.cleanup)
        self.out = Path(self.tmp.name).resolve() / 'run'

    def test_main_path_stale_refusal_survives_a_dead_stdout(self):
        """A closed stdout keeps the in-window refusal at exit 2."""
        class ClosedStdout:
            def write(self, data):
                raise OSError('stdout is closed')

            def flush(self):
                pass

        def fake_mutating(root, paths, **kwargs):
            class Guard:
                def __enter__(self):
                    return {path: b'fn unrelated() {}' for path in paths}

                def __exit__(self, *exc):
                    return False

            return Guard()

        runner = self.runner
        with patch.dict(os.environ, {'CARGO_TARGET_DIR': self.tmp.name,
                                     'NEGATIVE_ONLY': '', 'NEGATIVE_BASELINE': ''}), \
             patch.object(runner, 'mutating_sources', fake_mutating), \
             patch.object(sys, 'stdout', ClosedStdout()):
            with self.assertRaises(SystemExit) as caught:
                runner.main([str(Path(__file__).with_name('negative.py')), str(self.out)])
        message = '#1087 a dead stdout must keep the stale-anchor refusal at exit 2'
        self.assertTrue(caught.exception.code == 2,
                        message + ' (the refusal must stay exit 2, not crash rc=1)')
        # #1087 fix-r8 (S2-c): the report name is per-run
        # (``stale-anchors.<token>.log``).
        self.assertTrue(any(path.name.startswith('stale-anchors.') and path.name.endswith('.log')
                            for path in self.out.iterdir()),
                        message + ' (the refusal report must still reach the disk)')

    def test_check_anchors_stale_refusal_survives_a_dead_stdout(self):
        """A closed stdout keeps the read-only preflight at exit 2."""
        class ClosedStdout:
            def write(self, data):
                raise OSError('stdout is closed')

            def flush(self):
                pass

        runner = self.runner
        path = runner.mutations[0][1]
        fake = [('twin-stale', path, 'anchor twin: this text is not in any source',
                 'unused', runner.mutations[0][4])]
        with patch.object(runner, 'mutations', fake), \
             patch.object(sys, 'stdout', ClosedStdout()):
            with self.assertRaises(SystemExit) as caught:
                runner.main(['--check-anchors'])
        message = '#1087 the stale --check-anchors preflight must stay exit 2'
        self.assertTrue(caught.exception.code == 2,
                        message + ' (a dead stdout must not turn it into rc=1)')


if __name__ == '__main__':
    unittest.main()
