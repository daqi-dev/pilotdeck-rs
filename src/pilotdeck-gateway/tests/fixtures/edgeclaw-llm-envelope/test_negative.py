# anchor-selfcheck: unsupported unit tests of the replay kill judge, no product source mutations
#!/usr/bin/env python3
"""#1087 fix-r2 controls for the llm-envelope kill judge.

The delivered judge (``negative.py::classify``) is the shared #813 criterion:
the named test's own ``FAILED`` status line plus the failing oracle case's
``<fn> args=`` assertion message inside that test's own panic payload. The
controls below pin the positive shape and the false-kill shapes the old judge
(any non-zero exit with ``test result: FAILED.``) accepted — a sibling test's
panic, an anchor only in captured stdout, and an unrelated unwrap in the named
test. Every log is a synthetic libtest transcript; no cargo run happens here
(the real-sample kill is proven by the coordinator's full run).
``negative.py`` is a top-level script, so it is loaded through ``--list`` —
the one dispatch mode with no side effects.

Run with:

    python3 -B edgeclaw-llm-envelope/test_negative.py
"""
import importlib.util
import sys
from pathlib import Path
import unittest


def load_runner():
    source = Path(__file__).with_name('negative.py')
    spec = importlib.util.spec_from_file_location('llm_envelope_negative', source)
    module = importlib.util.module_from_spec(spec)
    argv = sys.argv
    sys.argv = [str(source), '--list']
    try:
        try:
            spec.loader.exec_module(module)
        except SystemExit:
            pass  # --list prints the names and exits 0; the judge is bound.
    finally:
        sys.argv = argv
    return module


class LlmEnvelopeJudgeControls(unittest.TestCase):
    """The llm-envelope kill judge must only accept the named test's anchor."""

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
            'tests/edgeclaw_llm_envelope.rs:12:5:\n'
            f'{payload}\n'
            'note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace\n'
            f'{summary}\n'
        )

    def judge(self, log, returncode, expected, test):
        return self.runner.classify(log, returncode, expected, test)

    def test_named_test_anchor_panic_kills(self):
        """The positive shape: the named test's own ``<fn> args=`` panic."""
        test = 'json_object_matches_ts'
        anchor = 'extractFirstJsonObject args='
        log = self._panic_log(
            test,
            'assertion `left == right` failed: '
            'extractFirstJsonObject args=["{\\"a\\":1}{\\"b\\":2}"] clock=0')
        self.assertTrue(self.judge(log, 101, anchor, test) == 'KILLED',
                        "#1087 the named test's own anchor panic must kill")

    def test_alternative_anchor_kills(self):
        """A group's alternative anchor still binds the kill to that test."""
        test = 'string_arrays_match_ts'
        anchor = ('normalizeStringArray args=', 'uniqueStrings args=')
        log = self._panic_log(
            test, 'assertion `left == right` failed: uniqueStrings args=[["a","a"]] clock=0')
        self.assertTrue(self.judge(log, 101, anchor, test) == 'KILLED',
                        '#1087 an alternative anchor of the group must kill')

    def test_captured_stdout_is_not_a_kill(self):
        """An anchor printed to captured stdout is not a kill."""
        test = 'json_object_matches_ts'
        anchor = 'extractFirstJsonObject args='
        log = (
            '\nrunning 1 test\n'
            f'test {test} ... FAILED\n'
            '\nfailures:\n'
            f'---- {test} stdout ----\n'
            'replay dump for debugging: extractFirstJsonObject args=[["x"]] clock=0\n'
            f"thread '{test}' (2841) panicked at "
            'tests/edgeclaw_llm_envelope.rs:44:9:\n'
            'called `Option::unwrap()` on a `None` value\n'
            'note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace\n'
            'test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out\n'
        )
        self.assertTrue(self.judge(log, 101, anchor, test) == 'INVALID',
                        '#1087 an anchor in captured stdout is not a kill')

    def test_sibling_panic_is_not_a_kill(self):
        """An anchor in a sibling test's panic is not a kill."""
        test = 'json_object_matches_ts'
        anchor = 'extractFirstJsonObject args='
        log = (
            '\nrunning 2 tests\n'
            f'test {test} ... ok\n'
            'test json_envelope_matches_ts ... FAILED\n'
            '\nfailures:\n'
            "thread 'json_envelope_matches_ts' (2841) panicked at "
            'tests/edgeclaw_llm_envelope.rs:44:9:\n'
            'assertion `left == right` failed: '
            'extractLooseJsonEnvelope args=["json {} end"] clock=0\n'
            'note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace\n'
            'test result: FAILED. 1 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out\n'
        )
        self.assertTrue(self.judge(log, 101, 'extractLooseJsonEnvelope args=', test)
                        == 'INVALID',
                        "#1087 a sibling test's panic is not a kill")

    def test_unrelated_unwrap_is_not_a_kill(self):
        """A FAILED run without the bound anchor message is not a kill."""
        test = 'json_object_matches_ts'
        anchor = 'extractFirstJsonObject args='
        log = self._panic_log(test, 'called `Option::unwrap()` on a `None` value')
        self.assertTrue(self.judge(log, 101, anchor, test) == 'INVALID',
                        '#1087 an unrelated unwrap panic is not a kill')

    def test_green_run_survives(self):
        """A passing bound test is SURVIVED, never INVALID."""
        test = 'json_object_matches_ts'
        anchor = 'extractFirstJsonObject args='
        log = self._panic_log(test, 'no failure', status='ok')
        self.assertTrue(self.judge(log, 0, anchor, test) == 'SURVIVED',
                        '#1087 a green bound test must read SURVIVED')

    def test_every_case_has_a_bound_anchor(self):
        """Every case name must carry an anchor in the EXPECTED table."""
        missing = [case[0] for case in self.runner.cases
                   if case[0] not in self.runner.EXPECTED]
        self.assertTrue(not missing,
                        f'#1087 every case must have a bound anchor: {missing}')


if __name__ == '__main__':
    unittest.main()
