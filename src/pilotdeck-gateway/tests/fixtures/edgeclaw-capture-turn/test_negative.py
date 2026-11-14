# anchor-selfcheck: unsupported unit tests of the restore-positive summary path, the full-table-only argument contract and the combined rejection anchor, no product source replacement table
"""#774 P2-1 / #786: runner contracts proven without touching product sources.

The restored-positive failure path (#774 P2-1), the full-table-only argument
contract (#786 fix-r2: the removed sample mode may not sneak back in) and the
combined rejection anchor (#786) are in-memory stubs: they never start cargo,
mutate a product source or touch the real evidence directory.
"""
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import tempfile
import types
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
            # #1087 fix-r8 (S2-b): the writer now names the run (token field
            # plus a unique staging name); #1087 fix-r9 (P2-a #2): the verdict
            # itself lands under the per-run name on this reused-mkdir
            # directory, not a shared fixed one.
            negative.write_summary(Path(tmp), payload, 'twin-token')
            summaries = list(Path(tmp).glob('summary.*.json'))
            self.assertEqual(len(summaries), 1,
                             '#1087 the verdict must land under its per-run name')
            summary = json.loads(summaries[0].read_text())
            self.assertEqual(summary['runToken'], 'twin-token',
                             '#1087 the summary must name the run it belongs to')
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
        # #1087 fix-r8 (S2-b): run() now returns (code, transcript) — the
        # verdict no longer reads the log back from disk — so the fake
        # runner hands back the pair.
        def failed(name, test):
            return 1, ''

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


class FullTableOnly(unittest.TestCase):
    """#786 fix-r2: the runner refuses per-mutant selections outright.

    The sample mode was removed because its verdict scaffolding kept producing
    false-pass/false-fail exit codes across two review rounds; a selection
    argument must therefore refuse to start instead of silently running a
    subset that could pose as full-table evidence.
    """

    def test_selection_argument_refuses_to_start(self):
        with patch.object(negative.sys, 'argv',
                          ['negative.py', 'out-dir', 'fork-carryover-filter-removed']):
            with self.assertRaises(SystemExit) as caught:
                negative.main()
        self.assertIn(
            'only executes the full table',
            str(caught.exception),
            '#786 fix-r2 a mutant-name argument must refuse to start the '
            'runner instead of selecting a sample')

    def test_missing_argument_refuses_to_start(self):
        with patch.object(negative.sys, 'argv', ['negative.py']):
            with self.assertRaises(SystemExit) as caught:
                negative.main()
        self.assertIn(
            'only executes the full table',
            str(caught.exception),
            '#786 fix-r2 a missing output directory must refuse to start')


class CombinedRejectionAnchor(unittest.TestCase):
    """#786-2: the no-error anchor must bind the test wording *and* the TS text."""

    mutant = dict(
        test='malformed_session_id_matches_ts_startswith_failure',
        anchor=negative.REJECT_ANCHOR,
        failure='panic',
    )

    def log(self, message):
        # #818 fix-r8 (rebase onto a8f55e769): synthetic logs carry the real
        # libtest shape, which the shared classifier now requires.
        return libtest_panic_log(self.mutant['test'], message)

    def test_both_halves_kill(self):
        raw = self.log(negative.REJECT_ANCHOR)
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


class InMemoryTranscriptVerdict(unittest.TestCase):
    """#1087 fix-r8 (S2-b): the verdict reads the transcript run_owned built.

    The OUT_DIR is a plain reused mkdir, and the old disk read-back could
    pick up another overlapping run's bytes on the log file and judge them
    (a false SURVIVED/KILLED). This twin poisons every on-disk ``.log``
    while the in-memory transcript stays whole: every verdict must see
    exactly the bytes its own ``run_owned`` call returned.
    """

    def test_poisoned_logs_never_reach_the_verdict(self):
        fixtures_root = FIXTURE.parents[4]
        guarded = {
            str(fixtures_root / 'src/pilotdeck-gateway/src/runtime/context/prepare.rs'),
            str(fixtures_root / 'src/pilotdeck-gateway/src/host/modules/manifest.rs'),
            str(FIXTURE / 'oracle.json'),
        }
        real_open = Path.open
        real_read_text = Path.read_text
        real_read_bytes = Path.read_bytes
        real_write_text = Path.write_text
        disk = {}
        transcripts = []
        recorded = []

        def fake_mutating_sources(root, files, **kwargs):
            originals = {path: real_read_bytes(path) for path in files}

            class Guard:
                def __enter__(self):
                    return originals

                def __exit__(self, *exc):
                    return False

            return Guard()

        def fake_run_owned(command, **kwargs):
            if command[-1:] == ['--exact']:
                test = command[-3]
                code, raw = 101, f'twin in-memory transcript for {test}\n'
            else:
                code, raw = 0, 'twin green positive run\n'
            transcripts.append(raw)
            return subprocess.CompletedProcess(command, code, raw, '')

        def fake_slot(script, **kwargs):
            return types.SimpleNamespace(stdout='', stderr='')

        def spy_classify(mutant, code, raw):
            recorded.append(raw)
            return 'KILLED', 'twin verdict'

        def poison_open(self, *args, **kwargs):
            handle = real_open(self, *args, **kwargs)
            if self.suffix != '.log':
                return handle

            class PoisonedLog:
                def __init__(self, inner):
                    self._inner = inner

                def write(self, data):
                    return self._inner.write('POISONED BY ANOTHER RUN\n')

                def __getattr__(self, name):
                    return getattr(self._inner, name)

                def __enter__(self):
                    return self

                def __exit__(self, *exc):
                    return self._inner.__exit__(*exc)

            return PoisonedLog(handle)

        def model_read_text(self, *args, **kwargs):
            if str(self) in disk:
                return disk[str(self)]
            return real_read_text(self, *args, **kwargs)

        def model_read_bytes(self, *args, **kwargs):
            if str(self) in disk:
                return disk[str(self)].encode()
            return real_read_bytes(self, *args, **kwargs)

        def model_write_text(self, data, *args, **kwargs):
            if str(self) in guarded:
                disk[str(self)] = data
                return None
            return real_write_text(self, data, *args, **kwargs)

        with tempfile.TemporaryDirectory(prefix='pdrs-1087-poison-') as temp:
            out = Path(temp) / 'run'
            argv = [str(FIXTURE / 'negative.py'), str(out)]
            with patch.dict(os.environ, {'CARGO_TARGET_DIR': str(temp)}), \
                 patch.object(negative.sys, 'argv', argv), \
                 patch.object(negative, 'mutating_sources', fake_mutating_sources), \
                 patch.object(negative, 'run_owned', fake_run_owned), \
                 patch.object(negative, 'wait_for_slot', fake_slot), \
                 patch.object(negative, 'classify_control', spy_classify), \
                 patch.object(Path, 'open', poison_open), \
                 patch.object(Path, 'read_text', model_read_text), \
                 patch.object(Path, 'read_bytes', model_read_bytes), \
                 patch.object(Path, 'write_text', model_write_text):
                negative.main()
            judged = [raw for raw in recorded]
            expected = [raw for raw in transcripts
                        if raw != 'twin green positive run\n']
        message = ('#1087 every verdict must read the in-memory transcript, '
                   'never the disk log')
        self.assertTrue(len(judged) > 0,
                        message + ' (no verdict was recorded at all)')
        self.assertEqual(judged, expected, message)


class TwoRunsOneDirectory(unittest.TestCase):
    """#1087 fix-r9 (P2-a #2): two runs on the reused OUT_DIR, two verdicts.

    The OUT_DIR is a plain reused mkdir (not a claim_dir exclusive
    directory), and before fix-r9 both runs published their verdict to the
    same fixed ``summary.json`` — the second run atomically replaced the
    first run's evidence. Both runs now land their own
    ``summary.<run_token>.json``; neither byte of the other's verdict may
    move. The mock手法 follows InMemoryTranscriptVerdict above: only the
    process layer is faked, the runner itself is the delivered script.
    """

    def test_neither_run_overwrites_the_other(self):
        fixtures_root = FIXTURE.parents[4]
        guarded = {
            str(fixtures_root / 'src/pilotdeck-gateway/src/runtime/context/prepare.rs'),
            str(fixtures_root / 'src/pilotdeck-gateway/src/host/modules/manifest.rs'),
            str(FIXTURE / 'oracle.json'),
        }
        real_open = Path.open
        real_read_text = Path.read_text
        real_read_bytes = Path.read_bytes
        real_write_text = Path.write_text
        disk = {}

        def fake_mutating_sources(root, files, **kwargs):
            originals = {path: real_read_bytes(path) for path in files}

            class Guard:
                def __enter__(self):
                    return originals

                def __exit__(self, *exc):
                    return False

            return Guard()

        def fake_run_owned(command, **kwargs):
            if command[-1:] == ['--exact']:
                code, raw = 101, 'twin in-memory transcript\n'
            else:
                code, raw = 0, 'twin green positive run\n'
            return subprocess.CompletedProcess(command, code, raw, '')

        def fake_slot(script, **kwargs):
            return types.SimpleNamespace(stdout='', stderr='')

        def model_read_text(self, *args, **kwargs):
            if str(self) in disk:
                return disk[str(self)]
            return real_read_text(self, *args, **kwargs)

        def model_read_bytes(self, *args, **kwargs):
            if str(self) in disk:
                return disk[str(self)].encode()
            return real_read_bytes(self, *args, **kwargs)

        def model_write_text(self, data, *args, **kwargs):
            if str(self) in guarded:
                disk[str(self)] = data
                return None
            return real_write_text(self, data, *args, **kwargs)

        with tempfile.TemporaryDirectory(prefix='pdrs-1087-tworuns-') as temp:
            out = Path(temp) / 'run'
            for run in range(2):
                argv = [str(FIXTURE / 'negative.py'), str(out)]
                with patch.dict(os.environ, {'CARGO_TARGET_DIR': str(temp)}), \
                     patch.object(negative.sys, 'argv', argv), \
                     patch.object(negative, 'mutating_sources', fake_mutating_sources), \
                     patch.object(negative, 'run_owned', fake_run_owned), \
                     patch.object(negative, 'wait_for_slot', fake_slot), \
                     patch.object(negative, 'classify_control',
                                  lambda mutant, code, raw: ('KILLED', 'twin verdict')), \
                     patch.object(Path, 'open', real_open), \
                     patch.object(Path, 'read_text', model_read_text), \
                     patch.object(Path, 'read_bytes', model_read_bytes), \
                     patch.object(Path, 'write_text', model_write_text):
                    negative.main()
            summaries = sorted(out.glob('summary.*.json'))
            message = ('#1087 two runs on one directory must publish two '
                       'distinct verdicts')
            self.assertEqual(len(summaries), 2,
                             message + f' (got {[p.name for p in summaries]})')
            self.assertFalse((out / 'summary.json').exists(),
                             message + ' (a fixed summary.json still exists)')
            payloads = [json.loads(path.read_text()) for path in summaries]
            tokens = {payload['runToken'] for payload in payloads}
            self.assertEqual(len(tokens), 2,
                             message + ' (both verdicts must name their own run)')
            for payload in payloads:
                self.assertEqual(payload['counts']['KILLED'],
                                 len(payload['results']),
                                 message + ' (each verdict must be a whole document)')
            self.assertEqual(len(list(out.glob('*.tmp'))), 0,
                             message + ' (a staging temp was left behind)')


if __name__ == '__main__':
    unittest.main()
