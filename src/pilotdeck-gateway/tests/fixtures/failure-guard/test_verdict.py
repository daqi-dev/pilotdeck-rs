"""In-memory unit tests for the failure-guard verdict branch (#813 fix-r11).

Run from this directory: ``python3 -m unittest test_verdict.py -v``.
Never imports ``negative.py``, runs cargo or mutates source bytes: the same
failed log is fed to a baseline and to a mutant, and the verdict branch alone
is asserted. The host runs the real sweep separately.
"""
from pathlib import Path
import sys
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_panic import named_test_killed
from verdict import Judge

PURE = 'runtime::turn::failure_guard::tests::real_ts_failure_guard_and_classify'
REPLAY = 'runtime::turn::submit::tests::real_ts_submit_scenarios'
MARKER = ('#813 failure guard trip: '
          'accumulate-success-does-not-reset-and-trip-latches action 4: '
          'trip result bytes must match')
BASELINE_PURE = dict(name='baseline-pure', test=PURE, marker=None)
BASELINE_SUBMIT = dict(name='baseline-submit', test=REPLAY, marker=None)
PURE_MUTANT = dict(name='tool-threshold-plus-one', test=PURE, marker=MARKER,
                   target=MARKER)
SUBMIT_MUTANT = dict(name='disabled-policy-observed', test=REPLAY,
                     marker=MARKER, target=MARKER)


def failure_log(test=PURE, marker=MARKER):
    """A real-shaped libtest failure whose marker is the mutant's target."""
    return f'''running 1 test
test {test} ... FAILED

failures:

failures:
    {test}

test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 2297 filtered out; finished in 0.05s

thread '{test}' (25086346) panicked at pilotdeck-gateway/src/runtime/turn/failure_guard_tests.rs:3:5:
assertion `left == right` failed: {marker}
  left: [1]
  right: [2]
note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace
error: test failed, to rerun pass `-p pilotdeck-gateway --lib`
'''


def pass_log(test=PURE):
    return f'''running 1 test
test {test} ... ok

test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.05s
'''


class BaselineDominatesMutants(unittest.TestCase):
    def test_failed_baseline_invalidates_same_test_mutant(self):
        # The shared kill rule alone accepts this log: the named test failed
        # and the marker is in its own panic payload. The baseline verdict is
        # what must turn the mutant INVALID, so this assertion proves the test
        # exercises the dependency rule and not a coincidental marker miss.
        raw = failure_log(PURE)
        self.assertTrue(named_test_killed(raw, PURE, MARKER, MARKER),
                        '#813 fix-r11: the mutant log must satisfy the shared kill rule')
        judge = Judge()
        valid, reason = judge.verdict(BASELINE_PURE, returncode=101,
                                      timed_out=False, raw=raw)
        self.assertFalse(valid,
                         '#813 fix-r11: the unmutated baseline must fail on this log')
        valid, reason = judge.verdict(PURE_MUTANT, returncode=101,
                                      timed_out=False, raw=raw)
        self.assertFalse(valid,
                         '#813 fix-r11: a failed baseline must invalidate its mutant')
        self.assertIn('baseline failed (baseline-pure)', reason,
                      '#813 fix-r11: reason must name the failed baseline')

    def test_pure_baseline_failure_does_not_invalidate_submit_mutant(self):
        judge = Judge()
        judge.verdict(BASELINE_PURE, returncode=101, timed_out=False,
                      raw=failure_log(PURE))
        valid, reason = judge.verdict(SUBMIT_MUTANT, returncode=101,
                                      timed_out=False,
                                      raw=failure_log(REPLAY))
        self.assertTrue(valid,
                        '#813 fix-r11: an unrelated baseline must not invalidate this mutant')
        self.assertEqual(reason, 'killed',
                         '#813 fix-r11: unrelated baseline leaves the kill intact')

    def test_submit_baseline_failure_does_not_invalidate_pure_mutant(self):
        judge = Judge()
        judge.verdict(BASELINE_SUBMIT, returncode=101, timed_out=False,
                      raw=failure_log(REPLAY))
        valid, reason = judge.verdict(PURE_MUTANT, returncode=101,
                                      timed_out=False,
                                      raw=failure_log(PURE))
        self.assertTrue(valid,
                        '#813 fix-r11: a failed submit baseline must not invalidate pure mutants')
        self.assertEqual(reason, 'killed',
                         '#813 fix-r11: pure mutant stays killed')

    def test_passed_baseline_keeps_the_mutant_killed(self):
        judge = Judge()
        valid, reason = judge.verdict(BASELINE_PURE, returncode=0,
                                      timed_out=False, raw=pass_log(PURE))
        self.assertTrue(valid, '#813 fix-r11: the baseline must pass its own log')
        self.assertEqual(reason, 'baseline passed',
                         '#813 fix-r11: passed baseline reason')
        valid, reason = judge.verdict(PURE_MUTANT, returncode=101,
                                      timed_out=False,
                                      raw=failure_log(PURE))
        self.assertTrue(valid, '#813 fix-r11: a passed baseline keeps the kill')
        self.assertEqual(reason, 'killed',
                         '#813 fix-r11: mutant reason stays killed')

    def test_deadline_timeout_baseline_invalidates_dependents(self):
        # A timeout is a failed baseline too; dependency must not depend on
        # the particular baseline failure reason.
        judge = Judge()
        valid, _ = judge.verdict(BASELINE_SUBMIT, returncode=-9,
                                 timed_out=True, raw='')
        self.assertFalse(valid,
                         '#813 fix-r11: a timed-out baseline is a failed baseline')
        valid, reason = judge.verdict(SUBMIT_MUTANT, returncode=101,
                                      timed_out=False,
                                      raw=failure_log(REPLAY))
        self.assertFalse(valid,
                         '#813 fix-r11: timeout baseline must invalidate its mutant')
        self.assertIn('baseline failed (baseline-submit)', reason,
                      '#813 fix-r11: timeout reason must name the failed baseline')


if __name__ == '__main__':
    unittest.main()
