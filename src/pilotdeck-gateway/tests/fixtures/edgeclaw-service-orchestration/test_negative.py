# anchor-selfcheck: unsupported unit tests of the replay kill judge, no product source mutations
#!/usr/bin/env python3
"""#1087 fix-r2 controls for the service-orchestration kill judge.

The delivered judge (``negative.py::classify``) is the shared #813 criterion:
the named test's own ``FAILED`` status line plus the mutant's anchor inside
that test's own panic payload. The controls below pin the positive shape and
the false-kill shapes the old judge (a whole-log ``anchor in log`` substring
plus a ``panicked at``-line filename scan) accepted — a sibling test's panic,
an anchor only in captured stdout, and an anchor inside build output. Every
log is a synthetic libtest transcript; no cargo run happens here (the
real-sample kill is proven by the coordinator's full run).

Run with:

    python3 -B edgeclaw-service-orchestration/test_negative.py
"""
import sys
from pathlib import Path
import subprocess
import unittest


class ServiceOrchestrationJudgeControls(unittest.TestCase):
    """The orchestration kill judge must only accept the named test's anchor."""

    def setUp(self):
        entry = str(Path(__file__).resolve().parent)
        if entry not in sys.path:
            sys.path.insert(0, entry)
            self.addCleanup(sys.path.remove, entry)
        import negative
        self.runner = negative

    TEST = 'rust_matches_ts_oracle'
    TEST_FILE = 'pilotdeck-gateway/tests/edgeclaw_service_orchestration.rs'

    @staticmethod
    def _panic_log(test, payload, *, status='FAILED', thread=None,
                   file='pilotdeck-gateway/tests/edgeclaw_service_orchestration.rs'):
        counts = ('0 passed; 1 failed' if status == 'FAILED' else '1 passed; 0 failed')
        summary = f'test result: {"FAILED" if status == "FAILED" else "ok"}. ' \
                  f'{counts}; 0 ignored; 0 measured; 0 filtered out'
        return (
            '\nrunning 1 test\n'
            f'test {test} ... {status}\n'
            '\nfailures:\n'
            f"thread '{thread or test}' (2841) panicked at "
            f'{file}:12:5:\n'
            f'{payload}\n'
            'note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace\n'
            f'{summary}\n'
        )

    def mutant(self, anchor, test=TEST, test_file=TEST_FILE):
        return dict(name='sample', path=None, old='', new='',
                    test=test, anchor=anchor, test_file=test_file, suite='test')

    def result(self, log, code=101):
        return subprocess.CompletedProcess([], code, '', log)

    def judge(self, log, code, anchor, test=TEST, test_file=TEST_FILE):
        return self.runner.classify(self.result(log, code),
                                     self.mutant(anchor, test, test_file))

    def test_named_test_anchor_panic_kills(self):
        """The positive shape: the named test's own anchor panic."""
        anchor = '#776 scenario dream-stage-act: sqlite differs from the real TS'
        log = self._panic_log(self.TEST, 'sqlite mismatch: ' + anchor)
        self.assertTrue(self.judge(log, 101, anchor)
                        == ('KILLED', 'bound test failed at its named assertion'),
                        "#1087 the named test's own anchor panic must kill")

    def test_captured_stdout_is_not_a_kill(self):
        """An anchor printed to captured stdout is not a kill."""
        anchor = '#776 scenario dream-stage-act: sqlite differs from the real TS'
        log = (
            '\nrunning 1 test\n'
            f'test {self.TEST} ... FAILED\n'
            '\nfailures:\n'
            f'---- {self.TEST} stdout ----\n'
            f'replay dump for debugging: {anchor}\n'
            f"thread '{self.TEST}' (2841) panicked at "
            f'{self.TEST_FILE}:44:9:\n'
            'called `Option::unwrap()` on a `None` value\n'
            'note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace\n'
            'test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out\n'
        )
        self.assertTrue(self.judge(log, 101, anchor)[0] == 'INVALID',
                        '#1087 an anchor in captured stdout is not a kill')

    def test_sibling_panic_is_not_a_kill(self):
        """An anchor in a sibling test's panic is not a kill."""
        anchor = '#776 scenario dream-stage-act: sqlite differs from the real TS'
        sibling = 'flush_second_completes_when_first_never_does'
        log = (
            '\nrunning 2 tests\n'
            f'test {self.TEST} ... ok\n'
            f'test {sibling} ... FAILED\n'
            '\nfailures:\n'
            f"thread '{sibling}' (2841) panicked at "
            'pilotdeck-gateway/tests/edgeclaw_snapshots.rs:44:9:\n'
            f'flush count wrong: {anchor}\n'
            'note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace\n'
            'test result: FAILED. 1 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out\n'
        )
        self.assertTrue(self.judge(log, 101, anchor)[0] == 'INVALID',
                        "#1087 a sibling test's panic is not a kill")

    def test_build_output_anchor_is_not_a_kill(self):
        """An anchor inside build output is never a kill."""
        anchor = '#776 scenario dream-stage-act: sqlite differs from the real TS'
        log = (
            f'error[E0308]: mismatched types in `{anchor}`\n'
            f' --> {self.TEST_FILE}:12:5\n'
            'error: could not compile `pilotdeck-gateway` '
            '(test "edgeclaw_service_orchestration") due to 1 previous error\n'
        )
        self.assertTrue(self.judge(log, 101, anchor)[0] == 'INVALID',
                        '#1087 an anchor inside build output is never a kill')

    def test_green_run_survives(self):
        """A passing bound test is SURVIVED, never INVALID."""
        anchor = '#776 scenario dream-stage-act: sqlite differs from the real TS'
        log = self._panic_log(self.TEST, 'no failure', status='ok')
        self.assertTrue(self.judge(log, 0, anchor) == ('SURVIVED', 'bound test still passed'),
                        '#1087 a green bound test must read SURVIVED')

    def test_every_mutant_has_a_bound_anchor(self):
        """Every mutant name must carry an anchor in the ANCHORS table."""
        missing = [mutant['name'] for mutant in self.runner.MUTANTS
                   if not mutant.get('anchor')]
        self.assertTrue(not missing,
                        f'#1087 every mutant must have a bound anchor: {missing}')


if __name__ == '__main__':
    unittest.main()
