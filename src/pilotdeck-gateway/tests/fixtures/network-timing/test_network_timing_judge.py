#!/usr/bin/env python3
# anchor-selfcheck: unsupported judge unit tests on in-memory libtest logs, no mutation table
"""#818 fix-r6: the full-suite branch keeps main's completed-run requirement.

`classify` must not call a run KILLED when the child was killed after printing
the bound cell's FAILED line and panic, or when the summary was truncated: a
kill needs libtest's own completion line (exit 101 + ``test result: FAILED.``).
"""
import importlib.util
from pathlib import Path
import types
import unittest

spec = importlib.util.spec_from_file_location(
    'network_timing_negative', Path(__file__).with_name('negative_controls.py'))
negative = importlib.util.module_from_spec(spec)
spec.loader.exec_module(negative)

CELL = 'fetch_redirect/headers/fulfilled/parent-first'
TEST = CELL.replace('/', '_').replace('-', '_')
ASSERTION = ('assertion `left == right` failed: network timing cell: '
             + CELL + ' #994 network-timing assertion')
COMPLETE = ('test result: FAILED. 148 passed; 23 failed; 22 ignored; 0 measured; '
            '0 filtered out; finished in 11.17s')
SINGLE = 'test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out'


def log(summary=COMPLETE):
    return ('running 193 tests\n'
            f'test {TEST} ... FAILED\n'
            f"thread '{TEST}' panicked at pilotdeck-gateway/tests/network_timing.rs:538:5:\n"
            f'{ASSERTION}\n'
            '\nfailures:\n'
            f'{summary}\n')


def run(code, raw):
    return types.SimpleNamespace(returncode=code, stdout=raw, stderr='')


class FullSuiteCompletionTests(unittest.TestCase):
    def test_complete_failure_kills(self):
        self.assertEqual(negative.classify('matrix-parent-abort', run(101, log()), '*'),
                         ('KILLED', 'bound network timing assertion failed'))

    def test_abnormal_exit_after_report_is_invalid(self):
        # P0 channel: the cell's FAILED line and panic are already printed when
        # the child is SIGKILL'ed/SIGABRT'ed before libtest's summary.
        for code in (-9, -6):
            with self.subTest(code=code):
                status, reason = negative.classify('matrix-parent-abort', run(code, log()), '*')
                self.assertEqual(status, 'INVALID', reason)

    def test_truncated_summary_is_invalid(self):
        status, reason = negative.classify(
            'matrix-parent-abort', run(101, log().split('test result:')[0]), '*')
        self.assertEqual(status, 'INVALID', reason)

    def test_non_failed_summary_is_invalid(self):
        status, reason = negative.classify(
            'matrix-parent-abort',
            run(101, log('test result: ok. 171 passed; 0 failed; 0 ignored')), '*')
        self.assertEqual(status, 'INVALID', reason)

    def test_noop_run_is_invalid(self):
        status, reason = negative.classify('matrix-parent-abort', run(0, ''), '*')
        self.assertEqual(status, 'INVALID', reason)


class SingleTestCompletionTests(unittest.TestCase):
    def test_complete_single_failure_kills(self):
        status, reason = negative.classify(
            'matrix-parent-abort', run(101, log(SINGLE)), TEST)
        self.assertEqual(status, 'KILLED', reason)

    def test_abnormal_single_exit_is_invalid(self):
        status, reason = negative.classify(
            'matrix-parent-abort', run(-9, log(SINGLE)), TEST)
        self.assertEqual(status, 'INVALID', reason)


if __name__ == '__main__':
    unittest.main()
