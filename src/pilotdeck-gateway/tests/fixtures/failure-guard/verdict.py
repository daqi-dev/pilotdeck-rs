# anchor-selfcheck: unsupported failure-guard verdict helper, no source replacement table
"""Verdicts for the failure-guard negative controls (#813 fix-r11).

``negative.py`` used to stop at the first invalid control. fix-r10 removed
that early exit so one host round exposes every mismatch, but also removed the
rule that a failing baseline suppresses mutant verdicts. The full sweep stays,
and the baseline rule is restored here: a control without a marker is an
unmutated baseline; once it fails, every later mutant that runs the same test
is INVALID, because replaying the unmutated failure cannot show that the
mutation caused it.

This module is deliberately pure: no cargo, no source bytes, no repository
walk. ``test_verdict.py`` replays the decision branch in memory, and
``negative.py`` is the only production consumer.
"""
from pathlib import Path
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_panic import named_test_killed


class Judge:
    """Per-run verdicts where a failed baseline dominates its dependents.

    Dependency is the control's full test name, not "the first baseline": a
    failing ``baseline-pure`` must not invalidate a submit mutant, and the
    reverse is equally true. Every control still runs so the log keeps the
    full diagnostic sweep; only the verdict changes.
    """

    def __init__(self, killed=named_test_killed):
        self._killed = killed
        self._failed_baselines = {}

    def verdict(self, control, *, returncode, timed_out, raw):
        """Return ``(valid, reason)`` for one control's finished run."""
        marker = control.get('marker')
        failed_baseline = self._failed_baselines.get(control['test'])
        if marker is None:
            valid = not timed_out and self._baseline_passed(control['test'],
                                                            returncode, raw)
            if not valid:
                # setdefault keeps the first failing baseline even if a later
                # duplicate row is somehow present; the name is the evidence.
                self._failed_baselines.setdefault(control['test'],
                                                  control['name'])
            reason = self._reason(control, marker, valid, timed_out,
                                  returncode, raw)
            return valid, reason
        if failed_baseline is not None:
            # The mutant may legitimately fail on the very assertion the
            # baseline already failed; that is not evidence of a kill.
            return False, (f'baseline failed ({failed_baseline}); '
                           'mutant cannot be KILLED')
        valid = not timed_out and self._mutant_killed(control, marker,
                                                      returncode, raw)
        return valid, self._reason(control, marker, valid, timed_out,
                                   returncode, raw)

    def _baseline_passed(self, test, returncode, raw):
        return (returncode == 0
                and 'test result: ok. 1 passed; 0 failed;' in raw
                and f'test {test} ... ok' in raw)

    def _mutant_killed(self, control, marker, returncode, raw):
        return (returncode == 101 and 'test result: FAILED' in raw
                and self._killed(raw, control['test'], marker,
                                 control.get('target'))
                and 'could not compile' not in raw)

    @staticmethod
    def _reason(control, marker, valid, timed_out, returncode, raw):
        if valid:
            return 'killed' if marker is not None else 'baseline passed'
        if timed_out:
            return 'timeout (watchdog), not an assertion failure'
        if 'could not compile' in raw:
            return 'compile error'
        if returncode == 0:
            return 'survived (test passed)'
        if returncode != 101:
            return f'unexpected exit {returncode}'
        return 'failed without the declared marker'
