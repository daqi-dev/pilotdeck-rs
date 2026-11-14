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
import os
import sys
import tempfile
from pathlib import Path
import unittest
from unittest.mock import patch


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

    #1087 fix-r3 (boundary #4): a throw case is killed only when the replay's
    own panic text names the expected error kind (``expected TypeError`` /
    ``expected RangeError``) inside the same payload — the recorded ``_throws``
    column is bound back into the judge. A panic carrying only the case id
    (the wrong-kind shape) is not the booked kill.

    #1087 fix-r4 (sibling #1): an aborted run is not a kill decision. A
    SIGINT/SIGKILL after the bound test's FAILED line leaves a partial
    transcript that the old judge (named test + payload alone) accepted —
    the summary line is the separate requirement pinning this closed.
    """

    def setUp(self):
        entry = str(Path(__file__).resolve().parent)
        if entry not in sys.path:
            sys.path.insert(0, entry)
            self.addCleanup(sys.path.remove, entry)
        import negative_shapes
        self.runner = negative_shapes

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
            'tests/edgeclaw_dream_helpers.rs:12:5:\n'
            f'{payload}\n'
            'note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace\n'
            f'{summary}\n'
        )

    def test_named_test_case_id_panic_kills(self):
        """The positive shape: the named test's own case-id panic."""
        test = 'mutation_matches_ts'
        log = self._panic_log(test, 'mutation/ applyMutation case=applyMutation#3: expected 2, got 3')
        self.assertTrue(self.runner.assertion_failure(log, test, 'applyMutation#3', None) is True,
                        '#1087 the named test\'s own case-id panic must kill')

    def test_expected_error_kind_panic_kills(self):
        """#1087 fix-r3 (boundary #4): the throw-case positive shape.

        A guard removed by a mutant makes the replay hit its own ``other``
        arm: the payload names the case id and the expected error kind.
        """
        test = 'clusters_match_ts'
        payload = ('clusters/ validateExclusiveClusters case='
                   'validateExclusiveClusters#12 expected TypeError '
                   '"Cannot read properties of undefined (reading \'map\')", '
                   'got Ok(...) args=...')
        log = self._panic_log(test, payload)
        self.assertTrue(
            self.runner.assertion_failure(log, test, 'validateExclusiveClusters#12',
                                           'TypeError') is True,
            '#1087 the expected-error-kind panic must kill the booked case')

    def test_wrong_error_kind_is_not_a_kill(self):
        """#1087 fix-r3 (boundary #4): the wrong error kind is not a kill.

        The case id is inside the named test's own panic payload, but the
        kind the oracle expects (TypeError) is not — the mutant flipped the
        error shape, which is a different contract than the one booked.
        """
        test = 'clusters_match_ts'
        payload = ('clusters/ validateExclusiveClusters case='
                   'validateExclusiveClusters#12 expected RangeError '
                   '"Maximum call stack size exceeded", got Ok(...) args=...')
        log = self._panic_log(test, payload)
        self.assertTrue(
            self.runner.assertion_failure(log, test, 'validateExclusiveClusters#12',
                                           'TypeError') is False,
            '#1087 a wrong-error-kind panic is not the booked kill')

    def test_case_id_without_expected_kind_is_not_a_kill(self):
        """#1087 fix-r3 (boundary #4): case id alone is not a throw-case kill.

        A kind-flip failure dies through the replay's ``assert_eq!`` on the
        error name: the custom message carries the case id but never names
        the expected kind.
        """
        test = 'clusters_match_ts'
        payload = ('assertion at tests/edgeclaw_dream_helpers.rs:296:21 failed: '
                   'clusters/ validateExclusiveClusters case='
                   'validateExclusiveClusters#12')
        log = self._panic_log(test, payload)
        self.assertTrue(
            self.runner.assertion_failure(log, test, 'validateExclusiveClusters#12',
                                           'TypeError') is False,
            '#1087 a case id without the expected kind is not the booked kill')

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
        self.assertTrue(self.runner.assertion_failure(log, test, 'applyMutation#3', None) is False,
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
        self.assertTrue(self.runner.assertion_failure(log, test, 'applyMutation#3', None) is False,
                        "#1087 a sibling test's panic is not a kill")

    def test_bare_assert_eq_without_case_id_is_not_a_kill(self):
        """A bare ``assert_eq!`` diff without the bound case id is not a kill."""
        test = 'mutation_matches_ts'
        log = self._panic_log(test, 'assertion `left == right` failed\n  left: 2\n right: 3')
        self.assertTrue(self.runner.assertion_failure(log, test, 'applyMutation#3', None) is False,
                        '#1087 a bare assert_eq! diff without the case id is not a kill')

    def test_aborted_run_without_summary_is_not_a_kill(self):
        """#1087 fix-r4 (sibling #1): a partial transcript is not a kill.

        The bound test already printed its FAILED line and its own payload
        (the ``--nocapture`` abort shape measured on a real libtest process
        killed by SIGKILL), but the run never reached libtest's own
        ``test result:`` summary. An aborted run is not a kill decision.
        """
        test = 'mutation_matches_ts'
        log = self._panic_log(test,
                              'mutation/ applyMutation case=applyMutation#3: expected 2, got 3',
                              summary='')
        self.assertTrue(self.runner.assertion_failure(log, test, 'applyMutation#3', None) is False,
                        '#1087 an aborted run without the FAILED summary line is not a kill')


class DreamRefusalControls(unittest.TestCase):
    """#1087 fix-r7 batch 1 (P1b): the dream stale-anchor refusal must survive a dead stdout.

    ``1>&-`` or a closed stream makes the in-window STALE ANCHOR prints
    raise; the old unguarded prints turned the never-started exit-2 refusal
    into an unhandled OSError crash (rc=1, "ran and was red"). No cargo run
    happens here — the process layer is patched on the imported module.
    """

    def setUp(self):
        entry = str(Path(__file__).resolve().parent)
        if entry not in sys.path:
            sys.path.insert(0, entry)
            self.addCleanup(sys.path.remove, entry)
        import negative
        self.runner = negative
        self.tmp = tempfile.TemporaryDirectory(prefix='dream-refusal-')
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
            # The dream refusal is a plain `return 2` out of main, so the
            # twin asserts the returned contract code, not a SystemExit.
            code = runner.main([str(Path(__file__).with_name('negative.py')), str(self.out)])
        message = '#1087 a dead stdout must keep the stale-anchor refusal at exit 2'
        self.assertTrue(code == 2,
                        message + ' (the refusal must stay exit 2, not crash rc=1)')
        # #1087 fix-r8 (S2-c): the report name is per-run
        # (``stale-anchors.<token>.log``); the tokenized glob keeps the twin
        # in sync with the shared naming.
        self.assertTrue(any(path.name.startswith('stale-anchors.') and path.name.endswith('.log')
                            for path in self.out.iterdir()),
                        message + ' (the refusal report must still reach the disk)')


class ShapesRefusalControls(unittest.TestCase):
    """#1087 fix-r7 batch 1 (P1b): the negative_shapes refusals must survive a dead stdout.

    All three refusal sites are pinned: the STALE TARGETS report, the
    read-only ``NEGATIVE_CHECK_ANCHORS`` precheck, and the in-window stale
    check. No cargo run happens here — the process layer is patched on the
    imported module.
    """

    def setUp(self):
        entry = str(Path(__file__).resolve().parent)
        if entry not in sys.path:
            sys.path.insert(0, entry)
            self.addCleanup(sys.path.remove, entry)
        import negative_shapes
        self.runner = negative_shapes
        self.tmp = tempfile.TemporaryDirectory(prefix='dream-shapes-refusal-')
        self.addCleanup(self.tmp.cleanup)
        self.out = Path(self.tmp.name).resolve() / 'run'

    def _env(self):
        return patch.dict(os.environ, {'CARGO_TARGET_DIR': self.tmp.name,
                                        'NEGATIVE_ONLY': '', 'NEGATIVE_BASELINE': ''})

    def test_stale_targets_refusal_survives_a_dead_stdout(self):
        """A closed stdout keeps the STALE TARGETS refusal at exit 2."""
        class ClosedStdout:
            def write(self, data):
                raise OSError('stdout is closed')

            def flush(self):
                pass

        runner = self.runner
        # An oracle with no cases at all makes every recorded signature a
        # stale target, so the refusal runs before any source is touched.
        with self._env(), \
             patch.object(runner, 'oracle', {'cases': []}), \
             patch.object(sys, 'stdout', ClosedStdout()):
            with self.assertRaises(SystemExit) as caught:
                runner.main([str(Path(__file__).with_name('negative_shapes.py')),
                             str(self.out)])
        message = '#1087 a dead stdout must keep the stale-targets refusal at exit 2'
        self.assertTrue(caught.exception.code == 2,
                        message + ' (the refusal must stay exit 2, not crash rc=1)')
        # #1087 fix-r8 (S2-c): the report name is per-run
        # (``stale-targets.<token>.log``).
        self.assertTrue(any(path.name.startswith('stale-targets.') and path.name.endswith('.log')
                            for path in self.out.iterdir()),
                        message + ' (the refusal report must still reach the disk)')

    def test_precheck_stale_refusal_survives_a_dead_stdout(self):
        """A closed stdout keeps the read-only precheck at its contract code."""
        class ClosedStdout:
            def write(self, data):
                raise OSError('stdout is closed')

            def flush(self):
                pass

        runner = self.runner
        # A drifted helpers.rs (read as unrelated bytes) makes every anchor
        # stale in the precheck; the prints must not turn the exit-2 refusal
        # into a crash.
        with self._env(), \
             patch.dict(os.environ, {'NEGATIVE_CHECK_ANCHORS': '1'}), \
             patch.object(Path, 'read_text', return_value='fn unrelated() {}'), \
             patch.object(sys, 'stdout', ClosedStdout()):
            with self.assertRaises(SystemExit) as caught:
                runner.main([str(Path(__file__).with_name('negative_shapes.py')),
                             str(self.out)])
        message = '#1087 a dead stdout must keep the precheck refusal at exit 2'
        self.assertTrue(caught.exception.code == 2,
                        message + ' (a dead stdout must not turn it into rc=1)')

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
        with self._env(), \
             patch.object(runner, 'mutating_sources', fake_mutating), \
             patch.object(sys, 'stdout', ClosedStdout()):
            with self.assertRaises(SystemExit) as caught:
                runner.main([str(Path(__file__).with_name('negative_shapes.py')),
                             str(self.out)])
        message = '#1087 a dead stdout must keep the in-window stale refusal at exit 2'
        self.assertTrue(caught.exception.code == 2,
                        message + ' (the refusal must stay exit 2, not crash rc=1)')
        # #1087 fix-r8 (S2-c): the report name is per-run
        # (``stale-anchors.<token>.log``); the tokenized glob keeps the twin
        # in sync with the shared naming.
        self.assertTrue(any(path.name.startswith('stale-anchors.') and path.name.endswith('.log')
                            for path in self.out.iterdir()),
                        message + ' (the refusal report must still reach the disk)')


if __name__ == '__main__':
    unittest.main()
