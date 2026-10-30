# anchor-selfcheck: unsupported unit tests of the restore-positive summary path and the in-memory sample-selection / combined-anchor contracts, no product source replacement table
"""#774 P2-1 / #786: runner contracts proven without touching product sources.

The restored-positive failure path (#774 P2-1), the optional sample selection
(#786) and the combined rejection anchor (#786) are in-memory stubs: they
never start cargo, mutate a product source or touch the real evidence
directory.
"""
import importlib.util
import json
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch

FIXTURE = Path(__file__).resolve().parent
SPEC = importlib.util.spec_from_file_location(
    'edgeclaw_capture_turn_negative', FIXTURE / 'negative.py')
negative = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(negative)


class RestoredPositiveFailure(unittest.TestCase):
    def test_timeout_still_writes_summary(self):
        def timed_out(name, test):
            raise subprocess.TimeoutExpired(['cargo', 'test'], 600)

        restored_ok, restored_error = negative.restore_positive(timed_out)
        self.assertFalse(restored_ok, '#774 restore timeout must not pass')
        self.assertIn('TimeoutExpired', restored_error,
                      '#774 restore timeout reason must name the exception')

        payload = negative.summary_payload(
            results=[dict(name='malformed-session-wrong-error-text',
                          status='KILLED')],
            failures=[],
            restored={'prepare.rs': True},
            restored_ok=restored_ok,
            restored_error=restored_error,
            before_sha256={'prepare.rs': 'before'},
            after_sha256={'prepare.rs': 'after'},
        )
        with tempfile.TemporaryDirectory(prefix='pdrs-774-negative-') as tmp:
            negative.write_summary(Path(tmp), payload)
            summary = json.loads((Path(tmp) / 'summary.json').read_text())
        self.assertFalse(summary['restoredPass'],
                         '#774 summary must record restoredPass=false')
        self.assertEqual(summary['restoredError'], restored_error,
                         '#774 summary must keep the restore failure reason')
        self.assertEqual(summary['counts'],
                         {'KILLED': 1, 'SURVIVED': 0, 'INVALID': 0},
                         '#774 summary must keep the classified mutants')

    def test_start_failure_still_has_reason(self):
        def missing_cargo(name, test):
            raise FileNotFoundError('cargo missing')

        restored_ok, restored_error = negative.restore_positive(missing_cargo)
        self.assertFalse(restored_ok,
                         '#774 restore start failure must not pass')
        self.assertIn('FileNotFoundError', restored_error,
                      '#774 restore start failure reason must name the exception')

    def test_nonzero_exit_has_reason(self):
        def failed(name, test):
            return 1

        restored_ok, restored_error = negative.restore_positive(failed)
        self.assertFalse(restored_ok, '#774 nonzero restore must not pass')
        self.assertEqual(restored_error, 'exit=1',
                         '#774 nonzero restore reason must carry the exit code')



def libtest_panic_log(test, payload, location=':1:1:'):
    """One real-shaped libtest run whose named test panics with ``payload``.

    #818: the shape libtest really prints -- a `running 1 test` header, the
    named test's own status line, and its `thread '<name>' panicked at`
    block -- because the classifiers read only the named test's own panic
    payload (#938 r14 evidence rules).
    """
    return (
        'running 1 test\n'
        f'test {test} ... FAILED\n'
        '\nfailures:\n'
        f'---- {test} stdout ----\n'
        f"thread '{test}' panicked at " + negative.TEST_FILE + location + ':\n'
        f'{payload}\n'
        'note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace\n'
        '\nfailures:\n'
        f'    {test}\n'
        '\ntest result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; '
        '0 filtered out;\n'
    )


class SharedVerdicts(unittest.TestCase):
    mutant = dict(test='bound_test', anchor='#778 witness', failure='assertion')
    log = libtest_panic_log('bound_test', 'assertion failed: #778 witness')

    def test_shared_invalid_is_preserved(self):
        with patch.object(negative, 'shared_classify', return_value=('INVALID', 'shared reason')) as call:
            self.assertEqual(negative.classify_control(self.mutant, 101, self.log), ('INVALID', 'shared reason'))
            result, bound = call.call_args.args
            self.assertEqual((result.returncode, result.stdout, bound.test, bound.assertion),
                             (101, self.log, 'bound_test', '#778 witness'))

    def test_bound_failure_and_invalid_counterexamples(self):
        self.assertEqual(negative.classify_control(self.mutant, 101, self.log)[0], 'KILLED',
                         '#778 a real-shaped bound failure must stay KILLED')
        counterexamples = [
            (self.log.replace(negative.TEST_FILE, 'unrelated.rs'), 'panic outside this fixture'),
            (self.log + 'could not compile', 'compile error'),
            (self.log.replace('#778 witness', 'other assertion'), 'unbound assertion text'),
            (self.log.replace('bound_test', 'other_test'), 'another test failed'),
            # #818 negative control: the marker on a bare panic line with no
            # named thread header is not the test's own payload, so the shared
            # classifier must reject it even though the marker text matches.
            (self.log.replace("thread 'bound_test' panicked", 'thread panicked'),
             'panic without the named test thread header'),
            # #818 negative control: a sibling test's panic block carrying the
            # same marker while the bound test passed is not evidence either.
            (self.log.replace('test bound_test ... FAILED', 'test bound_test ... ok')
                    .replace("thread 'bound_test' panicked", "thread 'other_test' panicked")
                    .replace('test result: FAILED. 0 passed; 1 failed',
                             'test result: FAILED. 1 passed; 1 failed'),
             'marker in a sibling test panic'),
        ]
        for raw, why in counterexamples:
            with self.subTest(why=why):
                self.assertEqual(negative.classify_control(self.mutant, 101, raw)[0], 'INVALID',
                                 f'#778 counterexample must stay INVALID: {why}')
        positive = 'test bound_test ... ok\ntest result: ok. 1 passed;\n'
        self.assertEqual(negative.classify_control(self.mutant, 0, positive)[0], 'SURVIVED')

    def test_timeout_payload_is_invalid(self):
        # #818 fix-r10 P1: merging main's shared-classify wrapper dropped the
        # pre-rebase strict judge, so the named test's own `#813 timeout:`
        # panic payload was judged KILLED. A watchdog/timeout hang is never
        # an assertion kill (negative_panic HANG_PREFIXES, #813 fix-r8) and
        # must stay INVALID even though the anchor text appears in it.
        mutant = dict(test='always_on_sessions_are_skipped',
                      anchor='must not reach the resolver', failure='panic')
        plain = libtest_panic_log(mutant['test'], mutant['anchor'], ':432:9:')
        hang = libtest_panic_log(mutant['test'],
                                 '#813 timeout: ' + mutant['anchor'], ':432:9:')
        self.assertEqual(negative.classify_control(mutant, 101, plain)[0],
                         'KILLED',
                         '#818 fix-r10: the plain anchor payload must stay KILLED')
        self.assertEqual(negative.classify_control(mutant, 101, hang)[0],
                         'INVALID',
                         '#818 fix-r10: a named-test timeout payload is a hang, '
                         'not an assertion kill')


class MutantSelection(unittest.TestCase):
    # Not named `mutants`: that is a mutation-table signal name for the
    # anchor selfcheck, and this in-memory stub owns no table.
    rows = [dict(name='alpha'), dict(name='beta')]

    def test_no_names_selects_everything(self):
        self.assertEqual(negative.selected_names(self.rows, []), set())

    def test_unknown_name_refuses(self):
        with self.assertRaises(SystemExit):
            negative.selected_names(self.rows, ['gamma'])

    def test_duplicate_name_refuses(self):
        with self.assertRaises(SystemExit):
            negative.selected_names(self.rows, ['alpha', 'alpha'])


class CombinedRejectionAnchor(unittest.TestCase):
    """#786-2: the no-error anchor must bind the test wording *and* the TS text."""

    mutant = dict(
        test='malformed_session_id_matches_ts_startswith_failure',
        anchor="#786 non-string sessionId must reject like isAlwaysOnSession (TS: reading 'startsWith')",
        failure='panic',
    )

    def log(self, message):
        # #818 fix-r8 (rebase onto a8f55e769): synthetic logs carry the real
        # libtest shape, which the shared classifier now requires.
        return libtest_panic_log(self.mutant['test'], message)

    def test_both_halves_kill(self):
        raw = self.log(
            "#786 non-string sessionId must reject like isAlwaysOnSession (TS: reading 'startsWith')")
        self.assertEqual(negative.classify_control(self.mutant, 101, raw)[0], 'KILLED')

    def test_rejection_wording_alone_is_invalid(self):
        raw = self.log('#786 non-string sessionId must reject like isAlwaysOnSession')
        self.assertEqual(negative.classify_control(self.mutant, 101, raw)[0], 'INVALID')

    def test_product_text_without_the_bound_panic_is_invalid(self):
        # The wrong-text mutant's failure carries the TS phrase, but not this
        # panic line; the combined anchor must not be satisfiable by it.
        raw = self.log(
            "#786 product error must contain the TS text reading 'startsWith': "
            '#774 wrong product error text')
        self.assertEqual(negative.classify_control(self.mutant, 101, raw)[0], 'INVALID')


if __name__ == '__main__':
    unittest.main()
