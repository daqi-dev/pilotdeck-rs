# anchor-selfcheck: unsupported unit tests of the replay kill judge, no product source mutations
#!/usr/bin/env python3
"""#1087 fix-r2 controls for the llm-config kill judge.

The delivered judge (``negative.py::classify``) is the shared #813 criterion:
the named test's own ``FAILED`` status line plus the bound anchor inside that
test's own panic payload. The controls below pin the positive shape and the
false-kill shapes the old judge (any non-zero exit with ``test result:
FAILED.``) accepted — a sibling test's panic, an anchor only in captured
stdout, and an anchor inside build output. Every log is a synthetic libtest
transcript; no cargo run happens here (the real-sample kill is proven by the
coordinator's full run). ``negative.py`` is a top-level script, so it is
loaded through ``--list`` — the one dispatch mode with no side effects.

Run with:

    python3 -B edgeclaw-llm-config/test_negative.py
"""
import importlib.util
import sys
from pathlib import Path
import unittest


def load_runner():
    source = Path(__file__).with_name('negative.py')
    spec = importlib.util.spec_from_file_location('llm_config_negative', source)
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


class LlmConfigJudgeControls(unittest.TestCase):
    """The llm-config kill judge must only accept the named test's anchor."""

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
            'tests/edgeclaw_llm_config.rs:12:5:\n'
            f'{payload}\n'
            'note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace\n'
            f'{summary}\n'
        )

    def judge(self, log, returncode, expected, test):
        return self.runner.classify(log, returncode, expected, test)

    TEST = 'llm_config_matches_ts'

    def test_named_test_anchor_panic_kills(self):
        """The positive shape: the named test's own ``$``-path anchor panic."""
        anchor = '$'
        log = self._panic_log(
            self.TEST, '$.provider: expected Some("p"), actual None')
        self.assertTrue(self.judge(log, 101, anchor, self.TEST) == 'KILLED',
                        "#1087 the named test's own anchor panic must kill")

    def test_special_anchors_kill(self):
        """Each special (non-``$``) anchor also binds to the named test."""
        anchors = ('$.autoIndexIntervalMinutes: number bits',
                   'second resolve blocked: the cache lock covers I/O',
                   '#836: cached bad-file null must not re-probe home')
        payloads = ('$.autoIndexIntervalMinutes: number bits',
                    'second resolve blocked: the cache lock covers I/O',
                    '#836: cached bad-file null must not re-probe home')
        for anchor, payload in zip(anchors, payloads):
            log = self._panic_log(self.TEST, payload)
            with self.subTest(anchor=anchor):
                self.assertTrue(self.judge(log, 101, anchor, self.TEST) == 'KILLED',
                                '#1087 the special anchor must kill')

    def test_captured_stdout_is_not_a_kill(self):
        """An anchor printed to captured stdout is not a kill."""
        anchor = '$'
        log = (
            '\nrunning 1 test\n'
            f'test {self.TEST} ... FAILED\n'
            '\nfailures:\n'
            f'---- {self.TEST} stdout ----\n'
            'replay dump for debugging: $.provider expected Some("p")\n'
            f"thread '{self.TEST}' (2841) panicked at "
            'tests/edgeclaw_llm_config.rs:44:9:\n'
            'called `Option::unwrap()` on a `None` value\n'
            'note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace\n'
            'test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out\n'
        )
        self.assertTrue(self.judge(log, 101, anchor, self.TEST) == 'INVALID',
                        '#1087 an anchor in captured stdout is not a kill')

    def test_sibling_panic_is_not_a_kill(self):
        """An anchor in a sibling test's panic is not a kill."""
        anchor = '$'
        sibling = 'llm_dream_matches_ts'
        log = (
            '\nrunning 2 tests\n'
            f'test {self.TEST} ... ok\n'
            f'test {sibling} ... FAILED\n'
            '\nfailures:\n'
            f"thread '{sibling}' (2841) panicked at "
            'tests/edgeclaw_llm_dream.rs:44:9:\n'
            '$.apiKey: expected Some("k"), actual None\n'
            'note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace\n'
            'test result: FAILED. 1 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out\n'
        )
        self.assertTrue(self.judge(log, 101, anchor, self.TEST) == 'INVALID',
                        "#1087 a sibling test's panic is not a kill")

    def test_build_output_anchor_is_not_a_kill(self):
        """An anchor inside build output is never a kill."""
        anchor = '$'
        log = (
            'error[E0308]: mismatched types at `$.provider`\n'
            ' --> tests/edgeclaw_llm_config.rs:12:5\n'
            'error: could not compile `pilotdeck-gateway` '
            '(test "edgeclaw_llm_config") due to 1 previous error\n'
        )
        self.assertTrue(self.judge(log, 101, anchor, self.TEST) == 'INVALID',
                        '#1087 an anchor inside build output is never a kill')

    def test_unrelated_unwrap_is_not_a_kill(self):
        """A FAILED run without the bound anchor is not a kill."""
        anchor = '$'
        log = self._panic_log(self.TEST, 'called `Option::unwrap()` on a `None` value')
        self.assertTrue(self.judge(log, 101, anchor, self.TEST) == 'INVALID',
                        '#1087 an unrelated unwrap panic is not a kill')

    def test_green_run_survives(self):
        """A passing bound test is SURVIVED, never INVALID."""
        anchor = '$'
        log = self._panic_log(self.TEST, 'no failure', status='ok')
        self.assertTrue(self.judge(log, 0, anchor, self.TEST) == 'SURVIVED',
                        '#1087 a green bound test must read SURVIVED')

    def test_every_case_has_a_bound_anchor(self):
        """Every case name must carry an anchor in the EXPECTED table."""
        missing = [case[0] for case in self.runner.cases
                   if case[0] not in self.runner.EXPECTED]
        self.assertTrue(not missing,
                        f'#1087 every case must have a bound anchor: {missing}')


if __name__ == '__main__':
    unittest.main()
