# anchor-selfcheck: unsupported unit tests of the settings kill judge, no product source mutations
#!/usr/bin/env python3
"""#1087 fix-r3 controls for the settings kill judge.

The delivered judge (``negative_controls.py::classify``) is the shared strict
#813 criterion: the named test's own ``FAILED`` status line plus the bound
assertion's message inside that test's own panic payload. The controls below
pin the positive shape and the false-kill shapes the old whole-log substring
check (``f'{test} ... FAILED' in log`` plus a loose fragment) accepted -- a
sibling test's panic, an anchor only in captured stdout, and an anchor inside
build output. Every log is a synthetic libtest transcript; no cargo run
happens here (the real-sample kill is proven by the coordinator's full run).

``negative_controls.py`` is a top-level script with no dispatch mode, so it
is loaded through the early refusal: an unknown control name makes ``select``
raise before anything else runs (exit 2), leaving the judge and its tables
bound in the module namespace.

The roster controls pin the sibling-review requirements: every fragment must
be a scoped marker (the three generic ``assertion `left == right` failed``
entries were replaced by dedicated messages in ``tests/settings.rs``), those
messages must really exist in the bound test file, and the cargo invocation
must pass ``--exact`` so the named test is the only one in the process.

Run with:

    python3 -B settings/test_negative.py
"""
import sys
from pathlib import Path
import unittest

FIXTURES = Path(__file__).resolve().parents[1]


def load_runner():
    """Exec the delivered script through its startup refusal; keep the judge."""
    source = Path(__file__).with_name('negative_controls.py')
    namespace = {'__name__': 'settings_negative_controls_twin',
                 '__file__': str(source)}
    argv = sys.argv
    # An unknown control name refuses at `select` (exit 2) before the target
    # checks or the evidence directory; every table and the judge stay bound.
    sys.argv = [str(source), 'unused-output', 'twin-unknown-control']
    try:
        try:
            exec(compile(source.read_text(encoding='utf-8'), str(source),
                         'exec'), namespace)
        except SystemExit as refusal:
            assert refusal.code == 2, f'unexpected refusal code {refusal.code}'
    finally:
        sys.argv = argv
    return namespace


class SettingsJudgeControls(unittest.TestCase):
    """The settings kill judge must only accept the named test's own panic."""

    @classmethod
    def setUpClass(cls):
        cls.runner = load_runner()
        if str(FIXTURES) not in sys.path:
            sys.path.insert(0, str(FIXTURES))
        from negative_scope import marker_is_scoped
        cls.marker_is_scoped = staticmethod(marker_is_scoped)

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
            'tests/settings.rs:12:5:\n'
            f'{payload}\n'
            'note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace\n'
            f'{summary}\n'
        )

    def judge(self, log, returncode, name):
        killed, failure = self.runner['classify'](returncode, log, name)
        return killed, failure

    def fragment(self, name):
        return self.runner['failure_fragments'][name]

    def test_named_test_fragment_panic_kills(self):
        """The positive shape, for every mutant in the roster."""
        tests, fragments = self.runner['tests'], self.runner['failure_fragments']
        for name in self.runner['EXPECTED_NAMES']:
            with self.subTest(mutant=name):
                payload = (f'assertion at tests/settings.rs:12:5 failed: '
                           f'{fragments[name]}')
                log = self._panic_log(tests[name], payload)
                killed, failure = self.judge(log, 101, name)
                message = '#1087 the named test\'s own fragment panic must kill'
                self.assertTrue(killed, message)
                self.assertIn(fragments[name], failure,
                              message + ' (the failure line must come from the payload)')

    def test_captured_stdout_is_not_a_kill(self):
        """A fragment echoed by the test's own stdout is not a kill."""
        name, test = 'redaction-disabled', self.runner['tests']['redaction-disabled']
        fragment = self.fragment('redaction-disabled')
        log = (
            '\nrunning 1 test\n'
            f'test {test} ... FAILED\n'
            '\nfailures:\n'
            f'---- {test} stdout ----\n'
            f'projection dump for debugging: {fragment}\n'
            f"thread '{test}' (2841) panicked at "
            'tests/settings.rs:44:9:\n'
            'assertion `left == right` failed\n'
            'note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace\n'
            'test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out\n'
        )
        killed, failure = self.judge(log, 101, name)
        message = '#1087 a fragment in captured stdout is not a kill'
        self.assertTrue(not killed, message)
        self.assertTrue(failure == '', message + ' (no failure line may be found)')

    def test_sibling_panic_is_not_a_kill(self):
        """A fragment in a sibling test's panic is not a kill."""
        name = 'top-level-key-order-swapped'
        test = self.runner['tests'][name]
        fragment = self.fragment(name)
        sibling = 'optional_fields_follow_truthiness'
        log = (
            '\nrunning 2 tests\n'
            f'test {test} ... ok\n'
            f'test {sibling} ... FAILED\n'
            '\nfailures:\n'
            f"thread '{sibling}' (2841) panicked at "
            'tests/settings.rs:44:9:\n'
            f'{fragment}\n'
            'note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace\n'
            'test result: FAILED. 1 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out\n'
        )
        killed, _ = self.judge(log, 101, name)
        self.assertTrue(not killed, '#1087 a sibling test\'s panic is not a kill')

    def test_build_output_fragment_is_not_a_kill(self):
        """A fragment inside build output is never a kill."""
        name = 'version-mistranscribed'
        fragment = self.fragment(name)
        log = (
            f'error[E0308]: mismatched types at `{fragment}`\n'
            ' --> tests/settings.rs:12:5\n'
            'error: could not compile `pilotdeck-gateway` '
            '(test "settings") due to 1 previous error\n'
        )
        killed, _ = self.judge(log, 101, name)
        self.assertTrue(not killed, '#1087 a fragment inside build output is not a kill')

    def test_green_run_is_not_a_kill(self):
        """A passing bound test is never a kill."""
        name, test = 'hint-always-included', self.runner['tests']['hint-always-included']
        log = self._panic_log(test, 'no failure', status='ok')
        killed, _ = self.judge(log, 0, name)
        self.assertTrue(not killed, '#1087 a green bound test is not a kill')

    def test_every_fragment_is_a_scoped_marker(self):
        """No fragment may be a generic whole-log payload again."""
        for name, fragment in self.runner['failure_fragments'].items():
            with self.subTest(mutant=name):
                self.assertTrue(self.marker_is_scoped(fragment),
                                f'#1087 the fragment for {name} must name one '
                                'assertion, not a class of failures')

    def test_bound_assertion_messages_exist_in_settings_rs(self):
        """Every fragment must really be an assertion message in the test file."""
        settings_rs = Path(__file__).resolve().parents[2] / 'settings.rs'
        text = settings_rs.read_text(encoding='utf-8')
        for name, fragment in self.runner['failure_fragments'].items():
            with self.subTest(mutant=name):
                self.assertIn(fragment, text,
                              f'#1087 the bound message for {name} is missing '
                              f'from {settings_rs}')

    def test_cargo_pins_the_named_test_with_exact(self):
        """The libtest filter must be exact: one test per process."""
        source = Path(__file__).with_name('negative_controls.py').read_text(
            encoding='utf-8')
        self.assertIn("'--', '--exact'", source,
                      '#1087 the cargo invocation must pass --exact so the '
                      'named test is the only test in the process')

    def test_every_mutant_is_bound_to_a_test_and_a_fragment(self):
        """Every roster name must carry both a bound test and a fragment."""
        tests, fragments = self.runner['tests'], self.runner['failure_fragments']
        missing = [name for name in self.runner['EXPECTED_NAMES']
                   if name not in tests or name not in fragments]
        self.assertTrue(not missing,
                        f'#1087 every mutant needs a bound test and fragment: {missing}')


if __name__ == '__main__':
    unittest.main()
