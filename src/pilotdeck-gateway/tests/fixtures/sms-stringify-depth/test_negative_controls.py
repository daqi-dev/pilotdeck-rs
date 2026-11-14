# anchor-selfcheck: unsupported unit tests of the sms kill judge, no product source mutations
#!/usr/bin/env python3
"""#1087 fix-r4 controls for the sms-stringify-depth kill judge.

The delivered runner (``negative_controls.py``) judges a mutant KILLED only
when libtest's own ``test result: FAILED.`` summary line is in the log, the
named test's own ``FAILED`` status line matches, and the bound ``#1929``
marker sits inside that test's own panic payload (#1087 fix-r4 same-family
sweep). The controls below pin that contract without running cargo: the
``killed`` expression is extracted verbatim from the delivered source and
evaluated against synthetic libtest transcripts.

* the positive shape (rc=101, named FAILED line, own payload with the
  ``#1929`` marker, summary line) is a kill;
* a green run is not a kill;
* an aborted run — rc=-9, the named test already printed its FAILED line and
  payload (the ``--nocapture`` SIGKILL shape measured on a real libtest
  process in ``/tmp/libtest-abort-probe-1087``) but libtest never reached
  its summary — is not a kill;
* the marker in a sibling test's payload does not kill.
"""
import ast
import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_panic import named_test_failed, named_test_killed

TEST = ('channel::sms::permission::tests::'
        'depth_past_threshold_truncates_without_panicking')
MARKER = '#1929 array chain at depth 256 must carry the truncation marker'


def libtest_panic_log(payload, *, test=TEST, summary='present', note=True):
    """A one-test libtest transcript with the test's own anchor panic."""
    if summary == 'present':
        summary = ('test result: FAILED. 0 passed; 1 failed; 0 ignored; '
                   '0 measured; 0 filtered out')
    lines = [
        'running 1 test',
        f'test {test} ... FAILED',
        '',
        'failures:',
        '',
        f'---- {test} stdout ----',
        f"thread '{test}' panicked at channel/sms/permission.rs:41:13:",
        payload,
    ]
    if note:
        lines.append('note: run with `RUST_BACKTRACE=1` environment variable '
                     'to display a backtrace')
    if summary:
        lines.append('')
        lines.append(summary)
    return '\n'.join(lines) + '\n'


def extract_kill_expression():
    """Pull the delivered `killed = (...)` expression verbatim."""
    source = Path(__file__).with_name('negative_controls.py')
    tree = ast.parse(source.read_text())
    for node in ast.walk(tree):
        if isinstance(node, ast.Assign) and any(
                target.id == 'killed' for target in node.targets
                if isinstance(target, ast.Name)):
            return ast.Expression(node.value)
    raise AssertionError('the killed assign is missing from negative_controls.py')


KILLED = compile(extract_kill_expression(), 'negative_controls.py:killed', 'eval')


def judge(code, log, test=TEST, marker=MARKER):
    return eval(KILLED, {'__builtins__': {}}, {
        'code': code, 'log': log, 'test': test, 'marker': marker,
        'named_test_failed': named_test_failed,
        'named_test_killed': named_test_killed,
    })


class SmsKillJudgeControls(unittest.TestCase):
    """The kill judge must only accept the named test's own #1929 assertion
    failure in a completed run."""

    def test_named_failure_with_libtest_summary_kills(self):
        log = libtest_panic_log(MARKER)
        self.assertTrue(
            judge(101, log),
            '#1087 the named #1929 assertion failure in a completed run kills')

    def test_green_run_does_not_kill(self):
        log = ('running 1 test\n'
               f'test {TEST} ... ok\n\ntest result: ok. 1 passed; 0 failed;\n')
        self.assertTrue(
            not judge(0, log),
            '#1087 a green run is never a kill')

    def test_aborted_run_without_summary_is_not_a_kill(self):
        log = libtest_panic_log(MARKER, summary=None)
        self.assertTrue(
            not judge(-9, log),
            '#1087 an aborted run without the FAILED summary line is not a kill')

    def test_marker_in_a_sibling_test_does_not_kill(self):
        sibling = TEST.rsplit('::', 1)[0] + '::wide_deep_payload_stays_bounded'
        log = (f'running 2 tests\n'
               f'test {TEST} ... ok\n'
               f'test {sibling} ... FAILED\n'
               '\nfailures:\n'
               f'\n---- {sibling} stdout ----\n'
               f"thread '{sibling}' panicked at channel/sms/permission.rs:41:13:\n"
               f'{MARKER}\n'
               'note: run with `RUST_BACKTRACE=1` environment variable to '
               'display a backtrace\n'
               '\ntest result: FAILED. 1 passed; 1 failed; 0 ignored; '
               '0 measured; 0 filtered out\n')
        self.assertTrue(
            not judge(101, log),
            '#1087 the marker must be in the named test\'s own payload')


if __name__ == '__main__':
    unittest.main(verbosity=2)
