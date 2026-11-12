# anchor-selfcheck: unsupported unit tests of the projector kill judge's overflow branch, no product source mutations
#!/usr/bin/env python3
"""#1087 fix-r4 controls for the agent-event-projector kill judge.

The delivered runner (``negative.py``) judges a stack-overflow mutant KILLED
on either of two disjuncts:

* the overflow disjunct — the named test's own "has overflowed its stack"
  report in the transcript is terminal abort-shaped evidence, so it needs no
  libtest summary line (a native stack overflow aborts before libtest could
  print one);
* the assertion disjunct — the named test's own ``FAILED`` status line with
  the ``should throw`` marker in that test's own panic payload, which since
  #1087 fix-r4 (same-family sweep) additionally requires libtest's own
  ``test result: FAILED`` summary: an aborted run that already printed the
  named test's FAILED line and payload (the ``--nocapture`` SIGKILL shape
  measured in ``/tmp/libtest-abort-probe-1087``) is not a kill.

The controls below extract the ``killed`` expression verbatim from the
delivered source and evaluate it against synthetic transcripts — no cargo.
"""
import ast
import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_panic import named_test_killed

TEST = ('agent_event_projector::tests::'
        'deep_tool_data_returns_ts_range_error_instead_of_overflowing_the_stack')
# The runner builds the full path from the test module; the transcript below
# carries whatever `used_test` the twin injects.


def libtest_panic_log(payload, *, test=TEST, summary='present'):
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
        f"thread '{test}' panicked at tests/projector.rs:41:13:",
        payload,
        'note: run with `RUST_BACKTRACE=1` environment variable to display '
        'a backtrace',
    ]
    if summary:
        lines.append('')
        lines.append(summary)
    return '\n'.join(lines) + '\n'


def extract_kill_expression(branch):
    """Pull a delivered `killed = (...)` assign verbatim.

    ``branch='overflow'`` selects the marker-tuple branch's assignment (it
    references the ``overflow`` helper); ``branch='marker'`` selects the
    plain-marker branch's assignment (it references ``marker``).
    """
    source = Path(__file__).with_name('negative.py')
    tree = ast.parse(source.read_text())
    for node in ast.walk(tree):
        if not (isinstance(node, ast.Assign) and any(
                target.id == 'killed' for target in node.targets
                if isinstance(target, ast.Name))):
            continue
        names = {child.id for child in ast.walk(node.value)
                 if isinstance(child, ast.Name)}
        if branch in names:
            return ast.Expression(node.value)
    raise AssertionError(f'the {branch}-branch killed assign is missing')


OVERFLOW_KILLED = compile(extract_kill_expression('overflow'),
                          'negative.py:killed-overflow', 'eval')
MARKER_KILLED = compile(extract_kill_expression('marker'),
                        'negative.py:killed-marker', 'eval')


def judge(returncode, raw, overflow):
    return eval(OVERFLOW_KILLED, {'__builtins__': {}}, {
        'returncode': returncode, 'raw': raw, 'overflow': overflow,
        'used_test': TEST, 'named_test_killed': named_test_killed,
    })


def judge_marker(returncode, raw, marker):
    return eval(MARKER_KILLED, {'__builtins__': {}}, {
        'returncode': returncode, 'raw': raw, 'used_test': TEST,
        'marker': marker, 'named_test_killed': named_test_killed,
    })


def overflow_of(raw):
    """The runner's own overflow test, computed the same way."""
    return 'has overflowed its stack' in raw and TEST in raw


class ProjectorOverflowJudgeControls(unittest.TestCase):
    """The overflow-branch kill judge: terminal overflow evidence still
    kills, the assertion path now requires the libtest summary."""

    def test_overflow_report_kills_without_a_libtest_summary(self):
        raw = (f'running 1 test\n'
               f'test {TEST} ... \n'
               '\nthread \'main\' has overflowed its stack\n'
               'fatal runtime error: stack overflow\n')
        self.assertTrue(
            judge(-6, raw, overflow_of(raw)),
            '#1087 the named stack overflow report is terminal evidence '
            'and stays a kill without the summary line')

    def test_named_assertion_with_libtest_summary_kills(self):
        raw = libtest_panic_log('should throw')
        self.assertTrue(
            judge(101, raw, overflow_of(raw)),
            '#1087 the named `should throw` assertion in a completed run '
            'kills')

    def test_aborted_assertion_without_summary_is_not_a_kill(self):
        raw = libtest_panic_log('should throw', summary=None)
        self.assertTrue(
            not judge(-9, raw, overflow_of(raw)),
            '#1087 an aborted run without the FAILED summary line is not '
            'an assertion kill')

    def test_compile_failure_does_not_kill(self):
        raw = ('error: could not compile `pilotdeck-gateway` '
               '(lib test) due to 1 previous error\n'
               'test result: FAILED. 0 passed; 1 failed;\n')
        self.assertTrue(
            not judge(101, raw, overflow_of(raw)),
            '#1087 a compile failure is excluded from both disjuncts')


class ProjectorPlainMarkerJudgeControls(unittest.TestCase):
    """#1087 fix-r5 (boundary #4): the plain-marker branch needs the summary.

    The non-tuple mutants' judge accepted the named test's FAILED status and
    panic payload without libtest's own summary line — the same aborted-run
    false-kill shape the fix-r4 sweep closed for the overflow branch.
    """

    MARKER = 'range error at the projector boundary'

    def test_named_marker_with_libtest_summary_kills(self):
        raw = libtest_panic_log(f'assertion failed: {self.MARKER}')
        self.assertTrue(
            judge_marker(101, raw, self.MARKER),
            '#1087 the named marker panic in a completed run kills')

    def test_aborted_marker_without_summary_is_not_a_kill(self):
        raw = libtest_panic_log(f'assertion failed: {self.MARKER}',
                                summary=None)
        self.assertTrue(
            not judge_marker(-9, raw, self.MARKER),
            '#1087 an aborted run without the FAILED summary line is not a '
            'kill in the plain-marker branch either')

    def test_compile_failure_does_not_kill(self):
        raw = ('error: could not compile `pilotdeck-gateway` '
               '(lib test) due to 1 previous error\n')
        self.assertTrue(
            not judge_marker(101, raw, self.MARKER),
            '#1087 a compile failure is excluded from the plain-marker branch')

    def test_unrelated_marker_is_not_a_kill(self):
        raw = libtest_panic_log('called `Option::unwrap()` on a `None` value')
        self.assertTrue(
            not judge_marker(101, raw, self.MARKER),
            '#1087 a FAILED run without the bound marker is not a kill')


if __name__ == '__main__':
    unittest.main(verbosity=2)
